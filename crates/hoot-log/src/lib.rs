//! hoot-log: a chip-agnostic, timestamped register-write log and writers for
//! the S98 v3 and VGM 1.71 file formats.
//!
//! The emulation side appends [`RegWrite`]s with timestamps in emulator ticks
//! (any rate, declared once in [`RegisterLog::ticks_per_second`]); the writers
//! convert to their native time base (1000 Hz syncs for S98, 44100 Hz samples
//! for VGM) without cumulative drift.
//!
//! Specs: S98 v3 <https://vgmrips.net/mirror/s98spec3.txt>,
//! VGM 1.71 <https://vgmrips.net/wiki/VGM_Specification>.

pub mod audible;
pub mod compare;
pub mod loops;
pub mod s98;
pub mod vgm;

pub use audible::{audibility, audibility_s98, markers, markers_s98, Audibility, Markers};
pub use compare::{compare, CompareReport};
pub use loops::detect_loop;
pub use s98::{read_s98, write_s98, ParsedS98};
pub use vgm::{write_vgm, write_vgz, Gd3};

/// Sound chips supported by the log. Only chips representable in at least one
/// of the two output formats belong here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chip {
    /// YM2149 / AY-3-8910 as configured SSG (S98 type 1)
    Ym2149,
    /// YM2203 OPN
    Ym2203,
    /// YM2612 OPN2
    Ym2612,
    /// YM2608 OPNA
    Ym2608,
    /// YM2151 OPM
    Ym2151,
    /// YM2413 OPLL
    Ym2413,
    /// YM3526 OPL
    Ym3526,
    /// YM3812 OPL2
    Ym3812,
    /// YMF262 OPL3
    Ymf262,
    /// AY-3-8910 PSG
    Ay8910,
    /// SN76489 DCSG
    Sn76489,
}

impl Chip {
    /// Does the chip have a second register port (address lines A1=1)?
    pub fn has_port1(self) -> bool {
        matches!(self, Chip::Ym2608 | Chip::Ym2612 | Chip::Ymf262)
    }
}

/// One chip instance in the log's device table.
#[derive(Debug, Clone)]
pub struct Device {
    pub chip: Chip,
    pub clock_hz: u32,
}

/// A register write: `addr`/`data` written to `port` (0 or 1) of `devices[dev]`
/// at absolute time `t` in log ticks.
#[derive(Debug, Clone, Copy)]
pub struct RegWrite {
    pub t: u64,
    pub dev: u8,
    pub port: u8,
    pub addr: u8,
    pub data: u8,
}

#[derive(Debug, Clone)]
pub struct RegisterLog {
    pub ticks_per_second: u64,
    pub devices: Vec<Device>,
    /// Must be sorted by `t` (appended in emulation order).
    pub writes: Vec<RegWrite>,
    /// Absolute end time of the piece in ticks (>= last write).
    pub end_t: u64,
    /// Loop start in ticks, if a loop was detected.
    pub loop_t: Option<u64>,
    /// VGM volume-modifier byte (header 0x7C). 0 = unity; 0xC1..=0xFF attenuate
    /// (each 0x20 below 0x100 halves), 0x01..=0xC0 boost. Set from a headroom
    /// target via [`vgm_volume_modifier`]. S98 has no equivalent field.
    pub volume_modifier: u8,
}

impl RegisterLog {
    pub fn new(ticks_per_second: u64, devices: Vec<Device>) -> Self {
        Self {
            ticks_per_second,
            devices,
            writes: Vec::new(),
            end_t: 0,
            loop_t: None,
            volume_modifier: 0,
        }
    }

    pub fn push(&mut self, w: RegWrite) {
        debug_assert!(self.writes.last().map(|p| p.t <= w.t).unwrap_or(true));
        self.end_t = self.end_t.max(w.t);
        self.writes.push(w);
    }

    /// Trim trailing silence: drop the tail wait so the piece ends `grace`
    /// seconds after the last register write (never past the current end).
    pub fn trim_trailing(&mut self, grace_secs: f64) {
        if let Some(last) = self.writes.last() {
            let grace = (grace_secs * self.ticks_per_second as f64) as u64;
            self.end_t = self.end_t.min(last.t.saturating_add(grace));
        }
    }
}

/// VGM volume-modifier byte that attenuates output by `headroom_db` dB (a
/// positive number reduces level). Each 0x20 step is 6.02 dB (a halving);
/// the field bottoms out near −11.8 dB (0xC1). 0 dB → 0 (unity).
pub fn vgm_volume_modifier(headroom_db: f32) -> u8 {
    if headroom_db <= 0.0 {
        return 0;
    }
    // multiplier = 10^(-headroom/20) = 2^(vm/0x20)  ⇒  vm = -headroom * (32/6.0206)
    let vm = (-headroom_db * (32.0 / 6.020_6)).round() as i32;
    let vm = vm.clamp(-63, 0); // attenuation only; 0xC1..=0xFF range
    if vm == 0 {
        0
    } else {
        (256 + vm) as u8 // e.g. -32 → 0xE0 (−6 dB)
    }
}

/// Convert an absolute tick time to an absolute time in `rate` units,
/// rounding to nearest. u128 math keeps 64-bit tick clocks overflow-free.
pub(crate) fn to_units(t: u64, tps: u64, rate: u64) -> u64 {
    ((t as u128 * rate as u128 + (tps as u128 / 2)) / tps as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversion_rounds() {
        // 1 second of 8 MHz ticks → exactly 44100 samples
        assert_eq!(to_units(8_000_000, 8_000_000, 44100), 44100);
        // half a sample rounds up
        assert_eq!(to_units(1, 2, 1), 1);
        assert_eq!(to_units(1, 3, 1), 0);
    }
}
