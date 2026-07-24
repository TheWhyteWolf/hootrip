//! Loop detection for register-write logs.
//!
//! PC-98 / PC-88 sound drivers replay a looping song from a fixed seam, and the
//! chip is deterministic: every loop pass emits an *identical* value-sequence of
//! register writes (only the absolute timestamps advance). So a loop shows up as
//! a period `L` where the write stream repeats exactly. We find it by matching a
//! signature of the last `K` writes against the stream one period earlier — which
//! needs only the capture to run into the second pass by `K` writes, not two full
//! passes.

use crate::RegisterLog;

/// Value-key of a write ignoring time: identical driver state → identical key.
#[inline]
fn key(w: &crate::RegWrite) -> u32 {
    (w.dev as u32) | ((w.port as u32) << 8) | ((w.addr as u32) << 16) | ((w.data as u32) << 24)
}

/// Detect a loop in `log`. Returns `(loop_start, loop_end)` indices into
/// `log.writes` such that the song plays `writes[0..loop_end]` and then repeats
/// `writes[loop_start..loop_end]` forever. The loop must last at least
/// `min_secs` seconds (filters short phrase-repeats and idle re-triggers).
pub fn detect_loop(log: &RegisterLog, min_secs: f64) -> Option<(usize, usize)> {
    let w = &log.writes;
    let n = w.len();
    // Signature length; must be shorter than the shortest plausible loop.
    const K: usize = 256;
    if n < 2 * K {
        return None;
    }
    let min_ticks = (min_secs * log.ticks_per_second as f64) as u64;

    // Prefix polynomial hash over keys for O(1) range-equality checks. Wrapping
    // u64 arithmetic; every hash hit is verified exactly below, so collisions
    // only cost a wasted comparison, never a wrong answer.
    const B: u64 = 0x100000001B3; // FNV-ish odd multiplier
    let mut h = vec![0u64; n + 1];
    let mut pw = vec![1u64; n + 1];
    for i in 0..n {
        h[i + 1] = h[i].wrapping_mul(B).wrapping_add(key(&w[i]) as u64 + 1);
        pw[i + 1] = pw[i].wrapping_mul(B);
    }
    let range_hash = |a: usize, b: usize| h[b].wrapping_sub(h[a].wrapping_mul(pw[b - a]));
    let sig = range_hash(n - K, n);
    let verify = |a: usize, b: usize, len: usize| w[a..a + len].iter().map(key).eq(w[b..b + len].iter().map(key));
    // How far the +L periodicity extends back from the seam (in writes).
    let extent = |l: usize| {
        let mut c = 0usize;
        let mut i = n - l;
        while i > 0 && key(&w[i - 1]) == key(&w[i - 1 + l]) {
            c += 1;
            i -= 1;
        }
        c
    };

    // Smallest lag L (≥ min_secs) where the final K writes recur one period back
    // AND the periodicity extends back at least a full period — i.e. the loop
    // truly repeats ≥2×. That rejects spurious short sub-period matches (which
    // recur only near the tail) and lands on the fundamental period rather than
    // a harmonic (a harmonic kL is a larger lag, so the smallest survivor wins).
    let mut period = None;
    for lag in K..=(n - K) {
        let a = n - K - lag;
        if range_hash(a, a + K) == sig && verify(a, n - K, K) {
            let dur = w[n - K].t.saturating_sub(w[a].t);
            if dur >= min_ticks && extent(lag) >= lag {
                period = Some(lag);
                break;
            }
        }
    }
    let l = period?;

    // Walk the seam back to the true musical loop start (where the +L periodicity
    // begins — i.e. the first write of pass 1, just after the intro/setup).
    let mut s = n - l;
    while s > 0 && key(&w[s - 1]) == key(&w[s - 1 + l]) {
        s -= 1;
    }
    Some((s, s + l))
}

/// Apply a detected loop in place: set `loop_t`, trim the log to one clean loop
/// past the intro, and set `end_t` to the seam. Returns true if a loop was set.
pub fn apply_loop(log: &mut RegisterLog, min_secs: f64) -> bool {
    if let Some((start, end)) = detect_loop(log, min_secs) {
        let loop_t = log.writes[start].t;
        let end_t = log.writes[end].t; // one full loop after loop_t
        log.writes.truncate(end);
        log.loop_t = Some(loop_t);
        log.end_t = end_t;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chip, Device, RegWrite, RegisterLog};

    fn log_from(keys: &[(u8, u8)], step: u64) -> RegisterLog {
        let mut log = RegisterLog::new(1000, vec![Device { chip: Chip::Ym2203, clock_hz: 4_000_000 }]);
        let mut t = 0;
        for &(addr, data) in keys {
            log.push(RegWrite { t, dev: 0, port: 0, addr, data });
            t += step;
        }
        log.end_t = t;
        log
    }

    #[test]
    fn detects_exact_tail_loop() {
        // intro (300) + loop body (400) repeated ~1.6× ; each write 10 ticks.
        let intro: Vec<(u8, u8)> = (0..300).map(|i| (0x30, (i % 251) as u8)).collect();
        let body: Vec<(u8, u8)> = (0..400).map(|i| (0xA0, (i * 7 % 253) as u8)).collect();
        let mut keys = intro.clone();
        keys.extend_from_slice(&body);
        keys.extend_from_slice(&body);
        keys.extend_from_slice(&body[..300]); // partial second-pass overrun > K
        let mut log = log_from(&keys, 10);
        // loop body 400 writes × 10 ticks = 4000 ticks = 4.0 s at 1000 tps.
        assert!(apply_loop(&mut log, 2.0));
        assert_eq!(log.loop_t, Some(300 * 10));
        assert_eq!(log.writes.len(), 300 + 400); // intro + one clean loop
        assert_eq!(log.end_t, (300 + 400) as u64 * 10);
    }

    #[test]
    fn no_loop_when_non_repeating() {
        // LCG-driven bytes: no short period, so no loop should be found.
        let mut s: u32 = 0x1234_5678;
        let keys: Vec<(u8, u8)> = (0..2000)
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (0x30 | ((s >> 20) & 0x0f) as u8, (s >> 12) as u8)
            })
            .collect();
        let mut log = log_from(&keys, 10);
        assert!(!apply_loop(&mut log, 2.0));
        assert_eq!(log.loop_t, None);
    }

    #[test]
    fn rejects_loop_below_min_secs() {
        // intro + THREE loop passes (400 writes × 10 ticks = 4.0 s each), enough
        // for the ≥2-pass extent check. A 5.0 s minimum rejects the 4.0 s loop;
        // a 2.0 s minimum accepts it.
        let intro: Vec<(u8, u8)> = (0..300).map(|i| (0x30, (i % 251) as u8)).collect();
        let body: Vec<(u8, u8)> = (0..400).map(|i| (0xA0, (i * 7 % 253) as u8)).collect();
        let mut keys = intro.clone();
        for _ in 0..3 {
            keys.extend_from_slice(&body);
        }
        keys.extend_from_slice(&body[..300]);
        let mut log = log_from(&keys, 10);
        assert!(!apply_loop(&mut log.clone(), 5.0));
        assert!(apply_loop(&mut log, 2.0));
        assert_eq!(log.loop_t, Some(300 * 10));
    }
}
