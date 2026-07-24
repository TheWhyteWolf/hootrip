//! Register-stream comparison for ground-truth validation.
//!
//! Given two S98 logs of the same track (ours vs a reference — a real-hardware
//! hoot capture, or an NP2/other-ripper log), align them and report how closely
//! the register-write streams match. Timing is normalised to milliseconds so
//! logs at different sync rates compare directly.

use crate::s98::ParsedS98;

/// A device write reduced to (chip_op, addr, data) — the sync-independent core.
type WriteKey = (u8, u8, u8);

#[derive(Debug, Clone)]
pub struct CompareReport {
    pub ours_writes: usize,
    pub ref_writes: usize,
    /// Writes whose (op, addr, data) match in order, ignoring time.
    pub matched: usize,
    /// Longest common subsequence length of the two (op,addr,data) streams.
    pub lcs: usize,
    /// For matched-in-order writes, the mean |Δt| in milliseconds.
    pub mean_abs_dt_ms: f64,
    /// Median absolute time delta (ms) — robust to a few outliers.
    pub median_abs_dt_ms: f64,
    /// ref_time / our_time best-fit ratio over matched writes (tempo skew;
    /// 1.0 = identical tempo, >1 = we run fast relative to the reference).
    pub tempo_ratio: f64,
    /// Registers that differ most (by count) between the streams.
    pub divergent_addrs: Vec<(u8, usize)>,
}

fn events_ms(p: &ParsedS98) -> Vec<(f64, WriteKey)> {
    let ms = p.sync_secs * 1000.0;
    p.events
        .iter()
        .map(|(t, op, a, d)| (*t as f64 * ms, (*op, *a, *d)))
        .collect()
}

/// Compare two parsed S98 logs.
pub fn compare(ours: &ParsedS98, reference: &ParsedS98) -> CompareReport {
    let a = events_ms(ours);
    let b = events_ms(reference);

    // LCS over the (op,addr,data) keys, recording matched index pairs so we can
    // measure their time deltas. Classic DP; logs are a few thousand writes.
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i].1 == b[j].1 {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let lcs = dp[0][0] as usize;

    // Walk the DP to collect matched (our_t, ref_t) pairs.
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i].1 == b[j].1 {
            pairs.push((a[i].0, b[j].0));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }

    // Tempo ratio: best-fit slope ref_t ≈ k·our_t through matched pairs
    // (least squares through origin), using only pairs past t=0.
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for &(ot, rt) in &pairs {
        num += ot * rt;
        den += ot * ot;
    }
    let tempo_ratio = if den > 0.0 { num / den } else { 1.0 };

    // Time deltas after tempo normalisation.
    let mut dts: Vec<f64> = pairs
        .iter()
        .map(|&(ot, rt)| (rt - ot * tempo_ratio).abs())
        .collect();
    let mean_abs_dt_ms = if dts.is_empty() {
        0.0
    } else {
        dts.iter().sum::<f64>() / dts.len() as f64
    };
    dts.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let median_abs_dt_ms = dts.get(dts.len() / 2).copied().unwrap_or(0.0);

    // Divergent registers: address histogram of the symmetric difference.
    let mut ours_h = [0usize; 512];
    let mut ref_h = [0usize; 512];
    for (_, (op, addr, _)) in &a {
        ours_h[((*op as usize & 1) << 8) | *addr as usize] += 1;
    }
    for (_, (op, addr, _)) in &b {
        ref_h[((*op as usize & 1) << 8) | *addr as usize] += 1;
    }
    let mut divergent: Vec<(u8, usize)> = (0..512)
        .filter_map(|k| {
            let diff = ours_h[k].abs_diff(ref_h[k]);
            (diff > 0).then_some((k as u8, diff))
        })
        .collect();
    divergent.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
    divergent.truncate(12);

    CompareReport {
        ours_writes: n,
        ref_writes: m,
        matched: pairs.len(),
        lcs,
        mean_abs_dt_ms,
        median_abs_dt_ms,
        tempo_ratio,
        divergent_addrs: divergent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s98::{read_s98, write_s98, S98Tags};
    use crate::{Chip, Device, RegWrite, RegisterLog};

    fn make(writes: &[(u64, u8, u8, u8)], end: u64) -> ParsedS98 {
        let mut log = RegisterLog::new(
            1_000_000,
            vec![Device { chip: Chip::Ym2203, clock_hz: 3_993_600 }],
        );
        for &(t, _op, a, d) in writes {
            log.push(RegWrite { t, dev: 0, port: 0, addr: a, data: d });
        }
        log.end_t = end;
        read_s98(&write_s98(&log, &S98Tags::default()).unwrap()).unwrap()
    }

    #[test]
    fn identical_streams_match_perfectly() {
        let w = [(0u64, 0u8, 0x28u8, 0xF0u8), (100_000, 0, 0xA4, 0x22), (200_000, 0, 0x28, 0x00)];
        let p = make(&w, 300_000);
        let r = compare(&p, &p);
        assert_eq!(r.matched, 3);
        assert_eq!(r.lcs, 3);
        assert!(r.mean_abs_dt_ms < 1.0);
        assert!((r.tempo_ratio - 1.0).abs() < 1e-6);
    }

    #[test]
    fn detects_tempo_skew() {
        let ours = make(&[(0, 0, 0x28, 0xF0), (100_000, 0, 0x28, 0x00), (200_000, 0, 0x28, 0xF0)], 300_000);
        // reference runs 10% slower (times ×1.1)
        let refr = make(&[(0, 0, 0x28, 0xF0), (110_000, 0, 0x28, 0x00), (220_000, 0, 0x28, 0xF0)], 330_000);
        let r = compare(&ours, &refr);
        assert_eq!(r.matched, 3);
        assert!((r.tempo_ratio - 1.1).abs() < 0.02, "ratio {}", r.tempo_ratio);
        assert!(r.mean_abs_dt_ms < 1.0); // near-perfect after normalisation
    }

    #[test]
    fn detects_missing_writes() {
        let ours = make(&[(0, 0, 0x28, 0xF0), (100_000, 0, 0xA4, 0x22), (200_000, 0, 0x28, 0x00)], 300_000);
        let refr = make(&[(0, 0, 0x28, 0xF0), (200_000, 0, 0x28, 0x00)], 300_000);
        let r = compare(&ours, &refr);
        assert_eq!(r.ours_writes, 3);
        assert_eq!(r.ref_writes, 2);
        assert_eq!(r.lcs, 2);
        assert!(r.divergent_addrs.iter().any(|(a, _)| *a == 0xA4));
    }
}
