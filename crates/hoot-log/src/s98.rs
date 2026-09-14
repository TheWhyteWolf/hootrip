//! S98 v3 writer, per <https://vgmrips.net/mirror/s98spec3.txt>.
//!
//! S98 is a pure FM/PSG register log: 1 "sync" = timer_num/timer_den seconds
//! (we write 1/1000 — the 1000 Hz community standard; hoot's own logger only
//! manages the 10/1000 default). The format has no PCM/ADPCM support: OPNA
//! ADPCM writes are still logged as plain register writes (real S98 players
//! handle the register-side of ADPCM only if the sample data is resident,
//! which it never is — use VGM output for ADPCM material).

use anyhow::{bail, Result};

use crate::{to_units, Chip, RegisterLog};

/// S98 sync rate we emit (syncs per second).
pub const SYNC_HZ: u64 = 1000;

fn device_type(chip: Chip) -> Option<u32> {
    Some(match chip {
        Chip::Ym2149 => 1,
        Chip::Ym2203 => 2,
        Chip::Ym2612 => 3,
        Chip::Ym2608 => 4,
        Chip::Ym2151 => 5,
        Chip::Ym2413 => 6,
        Chip::Ym3526 => 7,
        Chip::Ym3812 => 8,
        Chip::Ymf262 => 9,
        Chip::Ay8910 => 15,
        Chip::Sn76489 => 16,
    })
}

/// Inverse of [`device_type`]: the chip an S98 device-type code names.
/// Unknown codes yield `None` rather than a guess.
pub fn chip_from_device_type(ty: u32) -> Option<Chip> {
    Some(match ty {
        1 => Chip::Ym2149,
        2 => Chip::Ym2203,
        3 => Chip::Ym2612,
        4 => Chip::Ym2608,
        5 => Chip::Ym2151,
        6 => Chip::Ym2413,
        7 => Chip::Ym3526,
        8 => Chip::Ym3812,
        9 => Chip::Ymf262,
        15 => Chip::Ay8910,
        16 => Chip::Sn76489,
        _ => return None,
    })
}

/// Byte offsets of the two header fields that bracket the command stream.
/// Exposed so callers can digest the dump region alone — the tag block that
/// follows it carries the per-track title, which would defeat any attempt to
/// tell whether two tracks decode to the same music.
pub const TAG_OFS_POS: usize = 0x10;
pub const DUMP_OFS_POS: usize = 0x14;

/// The dump region of an S98 file: the command stream, without the header,
/// device table, or trailing tag block. `None` if the header is unusable.
pub fn dump_region(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 0x20 || &bytes[0..3] != b"S98" {
        return None;
    }
    // Both fields sit inside the fixed 0x20-byte header checked above.
    let rd = |p: usize| u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
    let tag = rd(TAG_OFS_POS);
    let dump = rd(DUMP_OFS_POS);
    let end = if tag > 0 && tag <= bytes.len() { tag } else { bytes.len() };
    if dump == 0 || dump > end {
        return None;
    }
    Some(&bytes[dump..end])
}

/// UTF-8 tag block fields ("[S98]" tag collection). Standard keys:
/// title, artist, game, year, genre, comment, copyright, s98by, system.
#[derive(Debug, Default, Clone)]
pub struct S98Tags {
    pub entries: Vec<(String, String)>,
}

impl S98Tags {
    pub fn set(&mut self, key: &str, value: &str) {
        if !value.is_empty() {
            self.entries.push((key.to_string(), value.to_string()));
        }
    }
}

pub fn write_s98(log: &RegisterLog, tags: &S98Tags) -> Result<Vec<u8>> {
    if log.devices.is_empty() {
        bail!("register log has no devices");
    }
    if log.devices.len() > 64 {
        bail!("S98 supports at most 64 devices");
    }

    let mut out = Vec::with_capacity(0x80 + log.writes.len() * 3);

    // --- Header (0x20 bytes) + device table --------------------------------
    out.extend_from_slice(b"S983");
    push_u32(&mut out, 1); // timer numerator
    push_u32(&mut out, SYNC_HZ as u32); // timer denominator
    push_u32(&mut out, 0); // compressing (deprecated, must be 0)
    let tag_ofs_pos = out.len();
    push_u32(&mut out, 0); // offset to tag (patched later)
    let dump_ofs_pos = out.len();
    push_u32(&mut out, 0); // offset to dump (patched later)
    let loop_ofs_pos = out.len();
    push_u32(&mut out, 0); // offset to loop (patched later)
    push_u32(&mut out, log.devices.len() as u32);
    for d in &log.devices {
        let Some(ty) = device_type(d.chip) else {
            bail!("chip {:?} not representable in S98", d.chip);
        };
        push_u32(&mut out, ty);
        push_u32(&mut out, d.clock_hz);
        push_u32(&mut out, 0); // pan
        push_u32(&mut out, 0); // reserved
    }

    // --- Dump ---------------------------------------------------------------
    let dump_start = out.len() as u32;
    patch_u32(&mut out, dump_ofs_pos, dump_start);

    let tps = log.ticks_per_second;
    let loop_sync = log.loop_t.map(|t| to_units(t, tps, SYNC_HZ));
    let mut emitted: u64 = 0; // syncs emitted so far
    let mut loop_ofs: Option<u32> = None;

    for w in &log.writes {
        let target = to_units(w.t, tps, SYNC_HZ);
        emit_sync_upto(&mut out, &mut emitted, target, loop_sync, &mut loop_ofs);
        if loop_ofs.is_none() && loop_sync.map(|ls| emitted >= ls).unwrap_or(false) {
            loop_ofs = Some(out.len() as u32);
        }
        if w.dev as usize >= log.devices.len() {
            bail!("write references device {} not in table", w.dev);
        }
        let op = (w.dev as u8) * 2 + (w.port & 1);
        out.extend_from_slice(&[op, w.addr, w.data]);
    }
    let end_target = to_units(log.end_t, tps, SYNC_HZ);
    emit_sync_upto(&mut out, &mut emitted, end_target, loop_sync, &mut loop_ofs);
    out.push(0xFD); // end/loop marker

    if let Some(ofs) = loop_ofs {
        patch_u32(&mut out, loop_ofs_pos, ofs);
    }

    // --- Tag ----------------------------------------------------------------
    if !tags.entries.is_empty() {
        let tag_start = out.len() as u32;
        patch_u32(&mut out, tag_ofs_pos, tag_start);
        out.extend_from_slice(b"[S98]");
        out.extend_from_slice(&[0xEF, 0xBB, 0xBF]); // UTF-8 BOM
        for (k, v) in &tags.entries {
            out.extend_from_slice(k.as_bytes());
            out.push(b'=');
            out.extend_from_slice(v.as_bytes());
            out.push(0x0A);
        }
        out.push(0x00);
    }

    Ok(out)
}

/// Emit sync commands to advance from `emitted` to `target` syncs, splitting
/// at the loop sync so the loop offset can land exactly on a command boundary.
fn emit_sync_upto(
    out: &mut Vec<u8>,
    emitted: &mut u64,
    target: u64,
    loop_sync: Option<u64>,
    loop_ofs: &mut Option<u32>,
) {
    if let Some(ls) = loop_sync {
        if loop_ofs.is_none() && *emitted < ls && ls < target {
            emit_syncs(out, ls - *emitted);
            *emitted = ls;
            *loop_ofs = Some(out.len() as u32);
        }
    }
    if target > *emitted {
        emit_syncs(out, target - *emitted);
        *emitted = target;
    }
}

fn emit_syncs(out: &mut Vec<u8>, n: u64) {
    match n {
        0 => {}
        1 => out.push(0xFF),
        2..=3 => {
            // 0xFE encodes n+2, so 2 and 3 need 1-sync commands.
            for _ in 0..n {
                out.push(0xFF);
            }
        }
        _ => {
            out.push(0xFE);
            // variable-length 7-bit little-endian of (n - 2), MSB = continue
            let mut v = n - 2;
            loop {
                let mut byte = (v & 0x7F) as u8;
                v >>= 7;
                if v != 0 {
                    byte |= 0x80;
                }
                out.push(byte);
                if v == 0 {
                    break;
                }
            }
        }
    }
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn patch_u32(out: &mut [u8], pos: usize, v: u32) {
    out[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
}

/// A parsed S98 file (for A/B comparison against other rippers' logs).
#[derive(Debug, Default)]
pub struct ParsedS98 {
    /// Seconds per sync.
    pub sync_secs: f64,
    /// (device_type, clock) entries; empty means the v1/v2 default OPNA.
    pub devices: Vec<(u32, u32)>,
    /// (sync_time, device_op, addr, data) — device_op = device*2 + port.
    pub events: Vec<(u64, u8, u8, u8)>,
    /// Sync count of the loop point, if any.
    pub loop_at: Option<u64>,
}

/// Read an S98 file, versions 1-3.
pub fn read_s98(bytes: &[u8]) -> Result<ParsedS98> {
    if bytes.len() < 0x20 || &bytes[0..3] != b"S98" {
        bail!("not an S98 file");
    }
    let version = bytes[3];
    let rd32 = |p: usize| u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as u64;
    let mut num = rd32(0x04);
    let mut den = rd32(0x08);
    if num == 0 {
        num = 10;
    }
    if den == 0 {
        den = 1000;
    }
    let dump = rd32(0x14) as usize;
    let loop_ofs = rd32(0x18) as usize;

    let mut out = ParsedS98 {
        sync_secs: num as f64 / den as f64,
        ..Default::default()
    };
    if version == b'3' {
        let count = rd32(0x1C) as usize;
        for i in 0..count.min(64) {
            let base = 0x20 + i * 16;
            if base + 16 > bytes.len() {
                bail!("device table truncated");
            }
            out.devices.push((rd32(base) as u32, rd32(base + 4) as u32));
        }
    }

    let mut i = dump;
    let mut t: u64 = 0;
    let mut loop_t: Option<u64> = None;
    while i < bytes.len() {
        if loop_ofs != 0 && i == loop_ofs {
            loop_t = Some(t);
        }
        match bytes[i] {
            0xFD => break,
            0xFF => {
                t += 1;
                i += 1;
            }
            0xFE => {
                i += 1;
                let mut v: u64 = 0;
                let mut sh = 0;
                loop {
                    if i >= bytes.len() {
                        bail!("truncated sync at {i:#x}");
                    }
                    let b = bytes[i];
                    i += 1;
                    v |= ((b & 0x7F) as u64) << sh;
                    sh += 7;
                    if b & 0x80 == 0 {
                        break;
                    }
                }
                t += v + 2;
            }
            op => {
                if i + 2 >= bytes.len() {
                    bail!("truncated write at {i:#x}");
                }
                out.events.push((t, op, bytes[i + 1], bytes[i + 2]));
                i += 3;
            }
        }
    }
    out.loop_at = loop_t;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Device, RegWrite};

    fn u32_at(b: &[u8], p: usize) -> u32 {
        u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
    }

    fn opna_log() -> RegisterLog {
        let mut log = RegisterLog::new(1_000_000, vec![Device { chip: Chip::Ym2608, clock_hz: 7_987_200 }]);
        // key-on at t=0, note change at 10ms, end at 1s
        log.push(RegWrite { t: 0, dev: 0, port: 0, addr: 0x28, data: 0xF0 });
        log.push(RegWrite { t: 10_000, dev: 0, port: 0, addr: 0xA4, data: 0x22 });
        log.push(RegWrite { t: 10_000, dev: 0, port: 1, addr: 0x01, data: 0x80 });
        log.end_t = 1_000_000;
        log
    }

    #[test]
    fn header_and_devices() {
        let bytes = write_s98(&opna_log(), &S98Tags::default()).unwrap();
        assert_eq!(&bytes[0..4], b"S983");
        assert_eq!(u32_at(&bytes, 0x04), 1); // 1/1000 s per sync
        assert_eq!(u32_at(&bytes, 0x08), 1000);
        assert_eq!(u32_at(&bytes, 0x1C), 1); // device count
        assert_eq!(u32_at(&bytes, 0x20), 4); // OPNA type
        assert_eq!(u32_at(&bytes, 0x24), 7_987_200);
        let dump = u32_at(&bytes, 0x14) as usize;
        assert_eq!(dump, 0x30);
        // dump: write, 10 syncs, 2 writes (one port1), 990 syncs, end
        assert_eq!(&bytes[dump..dump + 3], &[0x00, 0x28, 0xF0]);
        assert_eq!(&bytes[dump + 3..dump + 5], &[0xFE, 0x08]); // 10 syncs = 8+2
        assert_eq!(&bytes[dump + 5..dump + 8], &[0x00, 0xA4, 0x22]);
        assert_eq!(&bytes[dump + 8..dump + 11], &[0x01, 0x01, 0x80]); // port 1 op = 1
        // 990 syncs = 988+2; 988 = 7<<7 | 0x5C → 0xFE 0xDC 0x07
        assert_eq!(&bytes[dump + 11..dump + 14], &[0xFE, 0xDC, 0x07]);
        assert_eq!(bytes[dump + 14], 0xFD);
    }

    #[test]
    fn tags_written() {
        let mut tags = S98Tags::default();
        tags.set("title", "Test Song");
        tags.set("game", "The 4th Unit");
        let bytes = write_s98(&opna_log(), &tags).unwrap();
        let tag_ofs = u32_at(&bytes, 0x10) as usize;
        assert_ne!(tag_ofs, 0);
        assert_eq!(&bytes[tag_ofs..tag_ofs + 5], b"[S98]");
        assert_eq!(&bytes[tag_ofs + 5..tag_ofs + 8], &[0xEF, 0xBB, 0xBF]);
        let body = &bytes[tag_ofs + 8..];
        let text = std::str::from_utf8(&body[..body.len() - 1]).unwrap();
        assert!(text.contains("title=Test Song\n"));
        assert_eq!(*bytes.last().unwrap(), 0x00);
    }

    #[test]
    fn loop_offset_lands_on_boundary() {
        let mut log = opna_log();
        log.loop_t = Some(10_000); // loop from the 10ms writes
        let bytes = write_s98(&log, &S98Tags::default()).unwrap();
        let loop_ofs = u32_at(&bytes, 0x18) as usize;
        let dump = u32_at(&bytes, 0x14) as usize;
        // loop lands right after the initial write + 10 syncs
        assert_eq!(loop_ofs, dump + 5);
        assert_eq!(&bytes[loop_ofs..loop_ofs + 3], &[0x00, 0xA4, 0x22]);
    }

    #[test]
    fn read_roundtrip() {
        let mut log = opna_log();
        log.loop_t = Some(10_000);
        let mut tags = S98Tags::default();
        tags.set("title", "RT");
        let bytes = write_s98(&log, &tags).unwrap();
        let parsed = read_s98(&bytes).unwrap();
        assert_eq!(parsed.sync_secs, 0.001);
        assert_eq!(parsed.devices, vec![(4, 7_987_200)]);
        assert_eq!(
            parsed.events,
            vec![(0, 0, 0x28, 0xF0), (10, 0, 0xA4, 0x22), (10, 1, 0x01, 0x80)]
        );
        assert_eq!(parsed.loop_at, Some(10));
    }

    #[test]
    fn sync_encodings() {
        let mut out = Vec::new();
        emit_syncs(&mut out, 1);
        assert_eq!(out, [0xFF]);
        out.clear();
        emit_syncs(&mut out, 3);
        assert_eq!(out, [0xFF, 0xFF, 0xFF]);
        out.clear();
        emit_syncs(&mut out, 4);
        assert_eq!(out, [0xFE, 0x02]);
        out.clear();
        emit_syncs(&mut out, 2 + 0x80);
        assert_eq!(out, [0xFE, 0x80, 0x01]);
    }
}
