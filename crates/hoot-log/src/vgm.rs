//! VGM 1.71 writer, per <https://vgmrips.net/wiki/VGM_Specification>.
//!
//! Waits are in 1/44100 s samples. Up to two instances of each chip type are
//! supported (second instance via the 0xA1-0xAF mirror commands / bit 30 of
//! the clock field). OPNA ADPCM data blocks (0x67 type 0x81) are planned for
//! the PC-98 phase; the command layer here already reserves the hook.

use std::io::Write as _;

use anyhow::{bail, Result};
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::{to_units, Chip, RegisterLog};

pub const SAMPLE_HZ: u64 = 44100;
const HEADER_SIZE: usize = 0x100;
const VERSION_BCD: u32 = 0x0000_0171;
const DUAL_CHIP_BIT: u32 = 0x4000_0000;

/// GD3 1.00 tag block. All fields optional; English/Japanese pairs.
#[derive(Debug, Default, Clone)]
pub struct Gd3 {
    pub track_en: String,
    pub track_jp: String,
    pub game_en: String,
    pub game_jp: String,
    pub system_en: String,
    pub system_jp: String,
    pub author_en: String,
    pub author_jp: String,
    pub date: String,
    pub ripper: String,
    pub notes: String,
}

impl Gd3 {
    fn to_block(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for s in [
            &self.track_en,
            &self.track_jp,
            &self.game_en,
            &self.game_jp,
            &self.system_en,
            &self.system_jp,
            &self.author_en,
            &self.author_jp,
            &self.date,
            &self.ripper,
            &self.notes,
        ] {
            for u in s.encode_utf16() {
                body.extend_from_slice(&u.to_le_bytes());
            }
            body.extend_from_slice(&[0, 0]);
        }
        let mut block = Vec::with_capacity(12 + body.len());
        block.extend_from_slice(b"Gd3 ");
        block.extend_from_slice(&0x0000_0100u32.to_le_bytes());
        block.extend_from_slice(&(body.len() as u32).to_le_bytes());
        block.extend_from_slice(&body);
        block
    }
}

/// Header clock-field offset and (port0, port1) write commands per chip.
struct ChipSpec {
    clock_ofs: usize,
    cmd: [u8; 2],
    /// Second-instance commands (0 = unsupported / special-cased).
    cmd2: [u8; 2],
}

fn spec(chip: Chip) -> ChipSpec {
    match chip {
        Chip::Sn76489 => ChipSpec { clock_ofs: 0x0C, cmd: [0x50, 0], cmd2: [0x30, 0] },
        Chip::Ym2413 => ChipSpec { clock_ofs: 0x10, cmd: [0x51, 0], cmd2: [0xA1, 0] },
        Chip::Ym2612 => ChipSpec { clock_ofs: 0x2C, cmd: [0x52, 0x53], cmd2: [0xA2, 0xA3] },
        Chip::Ym2151 => ChipSpec { clock_ofs: 0x30, cmd: [0x54, 0], cmd2: [0xA4, 0] },
        Chip::Ym2203 => ChipSpec { clock_ofs: 0x44, cmd: [0x55, 0], cmd2: [0xA5, 0] },
        Chip::Ym2608 => ChipSpec { clock_ofs: 0x48, cmd: [0x56, 0x57], cmd2: [0xA6, 0xA7] },
        Chip::Ym3812 => ChipSpec { clock_ofs: 0x50, cmd: [0x5A, 0], cmd2: [0xAA, 0] },
        Chip::Ym3526 => ChipSpec { clock_ofs: 0x54, cmd: [0x5B, 0], cmd2: [0xAB, 0] },
        Chip::Ymf262 => ChipSpec { clock_ofs: 0x5C, cmd: [0x5E, 0x5F], cmd2: [0xAE, 0xAF] },
        // AY8910 and YM2149 share the AY header slot; type byte at 0x78.
        Chip::Ay8910 | Chip::Ym2149 => ChipSpec { clock_ofs: 0x74, cmd: [0xA0, 0], cmd2: [0xA0, 0] },
    }
}

fn ay_type(chip: Chip) -> u8 {
    match chip {
        Chip::Ay8910 => 0x00,
        Chip::Ym2149 => 0x10,
        _ => 0x00,
    }
}

pub fn write_vgm(log: &RegisterLog, gd3: &Gd3) -> Result<Vec<u8>> {
    // Map log devices to (chip, instance) and validate: VGM allows 2 per type.
    let mut instance = Vec::with_capacity(log.devices.len());
    for (i, d) in log.devices.iter().enumerate() {
        let slot = spec(d.chip).clock_ofs;
        let prior = log.devices[..i]
            .iter()
            .filter(|p| spec(p.chip).clock_ofs == slot)
            .count();
        if prior > 1 {
            bail!("VGM supports at most 2 instances of {:?}", d.chip);
        }
        instance.push(prior as u8);
    }

    let mut header = vec![0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(b"Vgm ");
    header[0x08..0x0C].copy_from_slice(&VERSION_BCD.to_le_bytes());
    header[0x34..0x38].copy_from_slice(&((HEADER_SIZE - 0x34) as u32).to_le_bytes());
    // Volume modifier (0x7C) — output headroom hint honoured by renderers.
    header[0x7C] = log.volume_modifier;

    for (i, d) in log.devices.iter().enumerate() {
        let s = spec(d.chip);
        let mut clock = d.clock_hz;
        if instance[i] == 1 {
            clock |= DUAL_CHIP_BIT;
        }
        // First instance writes the base clock; dual bit ORs on top.
        let existing = u32::from_le_bytes(header[s.clock_ofs..s.clock_ofs + 4].try_into().unwrap());
        let merged = if existing == 0 { clock } else { existing | DUAL_CHIP_BIT };
        header[s.clock_ofs..s.clock_ofs + 4].copy_from_slice(&merged.to_le_bytes());
        if matches!(d.chip, Chip::Ay8910 | Chip::Ym2149) {
            header[0x78] = ay_type(d.chip);
        }
    }

    // --- Command stream -----------------------------------------------------
    let tps = log.ticks_per_second;
    let loop_sample = log.loop_t.map(|t| to_units(t, tps, SAMPLE_HZ));
    let mut data = Vec::with_capacity(log.writes.len() * 3 + 16);
    let mut emitted: u64 = 0;
    let mut loop_data_ofs: Option<usize> = None;

    for w in &log.writes {
        let target = to_units(w.t, tps, SAMPLE_HZ);
        emit_wait_upto(&mut data, &mut emitted, target, loop_sample, &mut loop_data_ofs);
        if loop_data_ofs.is_none() && loop_sample.map(|ls| emitted >= ls).unwrap_or(false) {
            loop_data_ofs = Some(data.len());
        }
        let d = &log.devices[w.dev as usize];
        let s = spec(d.chip);
        match d.chip {
            Chip::Ay8910 | Chip::Ym2149 => {
                // Dual AY: second chip selected via bit 7 of the register byte.
                let reg = w.addr | if instance[w.dev as usize] == 1 { 0x80 } else { 0 };
                data.extend_from_slice(&[0xA0, reg, w.data]);
            }
            Chip::Sn76489 => {
                let cmd = if instance[w.dev as usize] == 1 { s.cmd2[0] } else { s.cmd[0] };
                data.extend_from_slice(&[cmd, w.data]);
            }
            _ => {
                let cmds = if instance[w.dev as usize] == 1 { s.cmd2 } else { s.cmd };
                let cmd = cmds[(w.port & 1) as usize];
                if cmd == 0 {
                    bail!("chip {:?} has no port {}", d.chip, w.port);
                }
                data.extend_from_slice(&[cmd, w.addr, w.data]);
            }
        }
    }
    let end_sample = to_units(log.end_t, tps, SAMPLE_HZ);
    emit_wait_upto(&mut data, &mut emitted, end_sample, loop_sample, &mut loop_data_ofs);
    data.push(0x66);

    header[0x18..0x1C].copy_from_slice(&(end_sample as u32).to_le_bytes());
    if let Some(ofs) = loop_data_ofs {
        let abs = HEADER_SIZE + ofs;
        header[0x1C..0x20].copy_from_slice(&((abs - 0x1C) as u32).to_le_bytes());
        let loop_samples = end_sample - loop_sample.unwrap();
        header[0x20..0x24].copy_from_slice(&(loop_samples as u32).to_le_bytes());
    }

    // --- Assemble -------------------------------------------------------------
    let gd3_block = gd3.to_block();
    let gd3_ofs = HEADER_SIZE + data.len();
    header[0x14..0x18].copy_from_slice(&((gd3_ofs - 0x14) as u32).to_le_bytes());
    let total = gd3_ofs + gd3_block.len();
    header[0x04..0x08].copy_from_slice(&((total - 4) as u32).to_le_bytes());

    let mut out = header;
    out.extend_from_slice(&data);
    out.extend_from_slice(&gd3_block);
    Ok(out)
}

/// gzip a VGM into a .vgz byte stream.
pub fn write_vgz(log: &RegisterLog, gd3: &Gd3) -> Result<Vec<u8>> {
    let vgm = write_vgm(log, gd3)?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&vgm)?;
    Ok(enc.finish()?)
}

fn emit_wait_upto(
    data: &mut Vec<u8>,
    emitted: &mut u64,
    target: u64,
    loop_sample: Option<u64>,
    loop_ofs: &mut Option<usize>,
) {
    if let Some(ls) = loop_sample {
        if loop_ofs.is_none() && *emitted < ls && ls < target {
            emit_waits(data, ls - *emitted);
            *emitted = ls;
            *loop_ofs = Some(data.len());
        }
    }
    if target > *emitted {
        emit_waits(data, target - *emitted);
        *emitted = target;
    }
}

fn emit_waits(data: &mut Vec<u8>, mut n: u64) {
    while n > 0 {
        match n {
            1..=16 => {
                data.push(0x70 + (n as u8 - 1));
                n = 0;
            }
            735 => {
                data.push(0x62);
                n = 0;
            }
            882 => {
                data.push(0x63);
                n = 0;
            }
            _ => {
                let chunk = n.min(65535) as u16;
                data.push(0x61);
                data.extend_from_slice(&chunk.to_le_bytes());
                n -= chunk as u64;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Device, RegWrite};

    fn u32_at(b: &[u8], p: usize) -> u32 {
        u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
    }

    fn opn_log() -> RegisterLog {
        let mut log = RegisterLog::new(
            44100,
            vec![
                Device { chip: Chip::Ym2203, clock_hz: 3_993_600 },
                Device { chip: Chip::Ym2149, clock_hz: 1_996_800 },
            ],
        );
        log.push(RegWrite { t: 0, dev: 0, port: 0, addr: 0x28, data: 0xF4 });
        log.push(RegWrite { t: 735, dev: 1, port: 0, addr: 0x07, data: 0x38 });
        log.end_t = 44100;
        log
    }

    #[test]
    fn header_fields() {
        let bytes = write_vgm(&opn_log(), &Gd3::default()).unwrap();
        assert_eq!(&bytes[0..4], b"Vgm ");
        assert_eq!(u32_at(&bytes, 0x08), 0x171);
        assert_eq!(u32_at(&bytes, 0x44), 3_993_600); // YM2203
        assert_eq!(u32_at(&bytes, 0x74), 1_996_800); // AY slot
        assert_eq!(bytes[0x78], 0x10); // AY type = YM2149
        assert_eq!(u32_at(&bytes, 0x34), 0x100 - 0x34); // data offset
        assert_eq!(u32_at(&bytes, 0x18), 44100); // total samples
        assert_eq!(u32_at(&bytes, 0x04) as usize, bytes.len() - 4); // EOF offset
    }

    #[test]
    fn command_stream() {
        let bytes = write_vgm(&opn_log(), &Gd3::default()).unwrap();
        let d = 0x100;
        assert_eq!(&bytes[d..d + 3], &[0x55, 0x28, 0xF4]); // YM2203 write
        assert_eq!(bytes[d + 3], 0x62); // 735-sample wait shortcut
        assert_eq!(&bytes[d + 4..d + 7], &[0xA0, 0x07, 0x38]); // AY write
        // remaining wait: 44100-735 = 43365 → 0x61 nnnn
        assert_eq!(bytes[d + 7], 0x61);
        assert_eq!(u16::from_le_bytes([bytes[d + 8], bytes[d + 9]]), 43365);
        assert_eq!(bytes[d + 10], 0x66);
    }

    #[test]
    fn gd3_utf16() {
        let mut gd3 = Gd3::default();
        gd3.track_en = "Opening".into();
        gd3.game_jp = "第4のユニット".into();
        let bytes = write_vgm(&opn_log(), &gd3).unwrap();
        let gd3_ofs = u32_at(&bytes, 0x14) as usize + 0x14;
        assert_eq!(&bytes[gd3_ofs..gd3_ofs + 4], b"Gd3 ");
        assert_eq!(u32_at(&bytes, gd3_ofs + 4), 0x100);
        let len = u32_at(&bytes, gd3_ofs + 8) as usize;
        assert_eq!(gd3_ofs + 12 + len, bytes.len());
        // "Opening" as UTF-16LE right after the 12-byte GD3 header
        assert_eq!(&bytes[gd3_ofs + 12..gd3_ofs + 14], &[b'O', 0]);
    }

    #[test]
    fn dual_chip_and_loop() {
        let mut log = RegisterLog::new(
            44100,
            vec![
                Device { chip: Chip::Ym2151, clock_hz: 4_000_000 },
                Device { chip: Chip::Ym2151, clock_hz: 4_000_000 },
            ],
        );
        log.push(RegWrite { t: 0, dev: 0, port: 0, addr: 0x08, data: 0x00 });
        log.push(RegWrite { t: 100, dev: 1, port: 0, addr: 0x08, data: 0x08 });
        log.end_t = 200;
        log.loop_t = Some(100);
        let bytes = write_vgm(&log, &Gd3::default()).unwrap();
        assert_eq!(u32_at(&bytes, 0x30), 4_000_000 | 0x4000_0000); // dual bit
        let d = 0x100;
        assert_eq!(bytes[d], 0x54); // first OPM
        // loop offset points at the second chip's write
        let loop_abs = u32_at(&bytes, 0x1C) as usize + 0x1C;
        assert_eq!(bytes[loop_abs], 0xA4); // second OPM command
        assert_eq!(u32_at(&bytes, 0x20), 100); // loop samples
    }

    #[test]
    fn vgz_roundtrip() {
        use std::io::Read;
        let vgm = write_vgm(&opn_log(), &Gd3::default()).unwrap();
        let vgz = write_vgz(&opn_log(), &Gd3::default()).unwrap();
        assert_eq!(&vgz[0..2], &[0x1F, 0x8B]); // gzip magic
        let mut dec = flate2::read::GzDecoder::new(&vgz[..]);
        let mut back = Vec::new();
        dec.read_to_end(&mut back).unwrap();
        assert_eq!(back, vgm);
    }
}
