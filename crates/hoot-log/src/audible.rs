//! Audibility classification for a captured register log.
//!
//! A rip that produced register writes has not necessarily produced *sound*.
//! Drivers routinely initialise the chip — timers, LFO, mode bits, a bank of
//! max-attenuation TLs — and then never sequence a note, because the song data
//! never reached RAM or the trigger never took. Those logs are perfectly
//! well-formed S98/VGM files that render to digital silence.
//!
//! The rules below were derived by rendering a random sample of ripped tracks
//! through libvgm and correlating the audio against the register stream: of the
//! tracks this module calls [`Audibility::Dead`], 30/30 rendered to pure zero;
//! of [`Audibility::NoVoice`], 15/15; of [`Audibility::Audible`], 0/30 were
//! silent.

use crate::s98::{chip_from_device_type, ParsedS98};
use crate::{Chip, RegisterLog};

/// Whether a log can produce audible output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audibility {
    /// Nothing was ever keyed on, no SSG tone was unmuted and no rhythm voice
    /// was triggered: the log renders to digital silence.
    Dead,
    /// Notes were keyed on, but no operator TL was ever programmed below
    /// maximum attenuation and neither SSG nor rhythm contributes — the voice
    /// data never loaded, so the log still renders to digital silence.
    NoVoice,
    /// The only activity is OPNA ADPCM-B control. Not a failed capture but an
    /// unrepresentable one: the sample data reaches the chip by DMA from host
    /// RAM, which a register log does not see. Measured over the 1,639
    /// ADPCM-only tracks in a full archive rip, *none* carried its samples
    /// inline (zero writes to the port-1 data register 0x08), so no such track
    /// is reproducible from the log alone.
    ///
    /// Silent in practice, with one trap: 635 of those 1,639 issue a START
    /// (reg 0x00 bit 7) over ADPCM RAM that was never filled, and a renderer
    /// will happily play that uninitialised memory as loud noise. Loud output
    /// here is garbage, not recovered music — which is why this class counts as
    /// silent rather than audible.
    AdpcmOnly,
    /// The log contains activity that can produce sound.
    Audible,
}

impl Audibility {
    /// Every non-[`Audible`](Audibility::Audible) class renders to silence
    /// with today's writers; only the reason differs.
    pub fn is_silent(self) -> bool {
        !matches!(self, Audibility::Audible)
    }

    /// Short tag for manifests and reports.
    pub fn tag(self) -> &'static str {
        match self {
            Audibility::Dead => "dead",
            Audibility::NoVoice => "novoice",
            Audibility::AdpcmOnly => "adpcm_only",
            Audibility::Audible => "audible",
        }
    }
}

/// Counts of the register activity that can result in audible output.
///
/// Accumulated once and shared by both callers — the live rip path (over a
/// [`RegisterLog`]) and the triage path (over an already-written S98) — so the
/// classification rules exist in exactly one place.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Markers {
    /// Writes to reg 0x28 with a non-empty slot mask.
    pub key_on: usize,
    /// SSG level regs 0x08..=0x0A written with a non-zero level.
    pub ssg_tone: usize,
    /// OPNA rhythm reg 0x10 written with a non-empty voice mask (not a dump).
    pub rhythm: usize,
    /// TL regs 0x40..=0x4F written below maximum attenuation.
    pub voiced: usize,
    /// OPNA ADPCM-B (DELTA-T) control writes, port 1 regs 0x00..=0x10.
    pub adpcm: usize,
    /// Latest value written to the SSG mixer (reg 0x07), if any. Scanning
    /// state, not a result.
    ssg_mixer: Option<u8>,
    /// Latched SSG channel levels (regs 0x08..=0x0A). Scanning state.
    ssg_level: [u8; 3],
    /// Has the SSG envelope shape (reg 0x0D) ever been written? Until it has,
    /// an envelope-mode level has nothing driving it. Scanning state.
    ssg_env: bool,
}

/// Does this chip use the OPN register map the rules below assume?
fn is_opn(chip: Chip) -> bool {
    matches!(chip, Chip::Ym2203 | Chip::Ym2608)
}

impl Markers {
    /// Fold one register write into the counts. Chips outside the OPN family
    /// contribute nothing — see [`audibility`] for how those logs are handled.
    pub fn feed(&mut self, chip: Chip, port: u8, addr: u8, data: u8) {
        if !is_opn(chip) {
            return;
        }
        if port == 0 {
            match addr {
                // Key-on: upper nibble is the slot mask; zero is a key-off.
                0x28 if data & 0xF0 != 0 => self.key_on += 1,
                // SSG mixer. Unmuting a channel that already has a level
                // latched makes it sound without any further level write.
                0x07 => {
                    self.ssg_mixer = Some(data);
                    self.rescan_ssg();
                }
                // SSG envelope shape. Writing it starts the envelope, which can
                // give voice to any channel already latched into envelope mode.
                0x0D => {
                    self.ssg_env = true;
                    self.rescan_ssg();
                }
                // SSG channel level.
                0x08..=0x0A => {
                    let ch = addr - 0x08;
                    self.ssg_level[ch as usize] = data;
                    if self.ssg_ch_sounds(ch) {
                        self.ssg_tone += 1;
                    }
                }
                // OPNA rhythm key-on; bit 7 set is the dump/mute form.
                0x10 if chip == Chip::Ym2608 && data & 0x3F != 0 && data & 0x80 == 0 => {
                    self.rhythm += 1
                }
                _ => {}
            }
        }
        // OPNA ADPCM-B lives at port 1 regs 0x00..=0x10; FM ch4-6 start at 0x30,
        // so the range is unambiguous.
        if port == 1 && addr <= 0x10 && chip == Chip::Ym2608 {
            self.adpcm += 1;
        }
        // Total level, both ports. 127 is silence; anything meaningfully below
        // it means a real voice was programmed.
        if (0x40..=0x4F).contains(&addr) && data & 0x7F < 120 {
            self.voiced += 1;
        }
    }

    /// Can SSG channel `ch` currently reach the output?
    ///
    /// Mixer bits are active-low: bit `ch` gates tone, bit `ch + 3` gates
    /// noise, and 0 means enabled. If the driver never wrote the mixer we have
    /// no evidence of muting, so the channel counts as live — this only ever
    /// demotes a channel on positive proof it was silenced.
    fn ssg_channel_live(&self, ch: u8) -> bool {
        match self.ssg_mixer {
            Some(m) => (m >> ch) & 1 == 0 || (m >> (ch + 3)) & 1 == 0,
            None => true,
        }
    }

    /// Can SSG channel `ch` currently make a sound?
    ///
    /// Bit 4 of the level register hands amplitude to the envelope generator
    /// and leaves the fixed level bits meaningless. A driver that parks a
    /// channel in envelope mode (0x10) without ever setting an envelope shape
    /// produces nothing — a real and common shape in silent rips.
    fn ssg_ch_sounds(&self, ch: u8) -> bool {
        let d = self.ssg_level[ch as usize];
        let amplitude = d & 0x0F != 0 || (d & 0x10 != 0 && self.ssg_env);
        amplitude && self.ssg_channel_live(ch)
    }

    /// Re-evaluate every channel after a change that can give voice to a level
    /// already latched (a mixer unmute, or the envelope starting).
    fn rescan_ssg(&mut self) {
        for ch in 0..3u8 {
            if self.ssg_ch_sounds(ch) {
                self.ssg_tone += 1;
            }
        }
    }

    /// Classify accumulated markers.
    pub fn classify(self) -> Audibility {
        // Neither SSG nor rhythm is contributing; the verdict rests on FM.
        let no_tonal = self.ssg_tone == 0 && self.rhythm == 0;
        if no_tonal && self.key_on == 0 {
            if self.adpcm > 0 { Audibility::AdpcmOnly } else { Audibility::Dead }
        } else if no_tonal && self.voiced == 0 {
            // Key-ons but no voice ever programmed. If the only content that
            // could make a sound is ADPCM, name it as such: a driver that keys
            // silent FM channels alongside its ADPCM is still an ADPCM track,
            // and calling it NoVoice hides that from the census.
            if self.adpcm > 0 { Audibility::AdpcmOnly } else { Audibility::NoVoice }
        } else {
            Audibility::Audible
        }
    }
}

/// Accumulate audibility markers over a register log.
pub fn markers(log: &RegisterLog) -> Markers {
    let mut m = Markers::default();
    for w in &log.writes {
        if let Some(d) = log.devices.get(w.dev as usize) {
            m.feed(d.chip, w.port, w.addr, w.data);
        }
    }
    m
}

/// Classify a register log.
///
/// Logs with no OPN-family device fall back to the original "did it write
/// anything at all" test: the rules here are OPN-specific, and a log for a chip
/// they do not describe must not be discarded on their say-so.
pub fn audibility(log: &RegisterLog) -> Audibility {
    if !log.devices.iter().any(|d| is_opn(d.chip)) {
        return if log.writes.is_empty() { Audibility::Dead } else { Audibility::Audible };
    }
    markers(log).classify()
}

/// Accumulate audibility markers over an already-parsed S98 file.
///
/// Same rules as [`markers`], applied to a log read back from disk — so the
/// triage pass over exported files and the live rip path cannot disagree.
pub fn markers_s98(parsed: &ParsedS98) -> Markers {
    let mut m = Markers::default();
    for &(_, op, addr, data) in &parsed.events {
        let dev = (op >> 1) as usize;
        let port = op & 1;
        let chip = match parsed.devices.get(dev) {
            Some(&(ty, _)) => chip_from_device_type(ty),
            // S98 v1/v2 carry no device table and are OPNA by definition.
            None if parsed.devices.is_empty() => Some(Chip::Ym2608),
            None => None,
        };
        if let Some(c) = chip {
            m.feed(c, port, addr, data);
        }
    }
    m
}

/// Classify an already-parsed S98 file, with the same non-OPN fallback as
/// [`audibility`].
pub fn audibility_s98(parsed: &ParsedS98) -> Audibility {
    let has_opn = if parsed.devices.is_empty() {
        true // v1/v2 default OPNA
    } else {
        parsed.devices.iter().any(|&(ty, _)| chip_from_device_type(ty).is_some_and(is_opn))
    };
    if !has_opn {
        return if parsed.events.is_empty() { Audibility::Dead } else { Audibility::Audible };
    }
    markers_s98(parsed).classify()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Device, RegWrite};

    fn log_with(chip: Chip, writes: &[(u8, u8, u8)]) -> RegisterLog {
        let clock = if chip == Chip::Ym2608 { 7_987_200 } else { 3_993_600 };
        let mut log = RegisterLog::new(1_000_000, vec![Device { chip, clock_hz: clock }]);
        for (i, &(port, addr, data)) in writes.iter().enumerate() {
            log.push(RegWrite { t: i as u64 * 1000, dev: 0, port, addr, data });
        }
        log
    }

    #[test]
    fn empty_log_is_dead() {
        assert_eq!(audibility(&log_with(Chip::Ym2203, &[])), Audibility::Dead);
    }

    #[test]
    fn init_churn_without_keyon_is_dead() {
        // Timer/mode setup and a bank of max-attenuation TLs: the exact shape
        // of the silent rips this gate exists to catch.
        let log = log_with(
            Chip::Ym2203,
            &[(0, 0x27, 0x30), (0, 0x24, 0x00), (0, 0x25, 0x00), (0, 0x40, 0x7F), (0, 0x41, 0x7F)],
        );
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn keyoff_alone_is_dead() {
        // Reg 0x28 with an empty slot mask is a key-*off* and makes no sound.
        let log = log_with(Chip::Ym2203, &[(0, 0x28, 0x00), (0, 0x28, 0x01)]);
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn keyon_without_voice_is_novoice() {
        let log = log_with(Chip::Ym2203, &[(0, 0x40, 0x7F), (0, 0x28, 0xF0), (0, 0x28, 0xF1)]);
        assert_eq!(audibility(&log), Audibility::NoVoice);
    }

    #[test]
    fn keyon_with_programmed_tl_is_audible() {
        let log = log_with(Chip::Ym2203, &[(0, 0x40, 0x20), (0, 0x28, 0xF0)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn tl_on_port1_counts_as_voiced() {
        let log = log_with(Chip::Ym2608, &[(1, 0x40, 0x18), (0, 0x28, 0xF4)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn ssg_tone_alone_is_audible() {
        // SSG-only music keys nothing on the FM side.
        let log = log_with(Chip::Ym2203, &[(0, 0x07, 0x38), (0, 0x08, 0x0C)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn ssg_level_behind_a_muting_mixer_is_not_tone() {
        // Mixer 0xBF: tone bits 0-2 and noise bits 3-5 all set = all muted.
        // Observed in the wild ("Dengeti Nurse" track 13), which renders to
        // pure digital zero despite 66 non-zero SSG level writes.
        let log = log_with(Chip::Ym2203, &[(0, 0x07, 0xBF), (0, 0x0A, 0x31)]);
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn unmuting_a_latched_ssg_level_is_tone() {
        // Level latched while muted, then the mixer opens channel 2's tone.
        let log = log_with(Chip::Ym2203, &[(0, 0x07, 0xBF), (0, 0x0A, 0x0C), (0, 0x07, 0xBB)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn ssg_noise_only_still_counts_as_tone() {
        // Tone muted (bit 2 set) but noise open (bit 5 clear) on channel 2.
        let log = log_with(Chip::Ym2203, &[(0, 0x07, 0x9F), (0, 0x0A, 0x0C)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn ssg_level_with_no_mixer_write_counts() {
        // No evidence of muting: must not be demoted.
        let log = log_with(Chip::Ym2203, &[(0, 0x08, 0x0C)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn ssg_envelope_mode_without_a_shape_is_silent() {
        // 0x10 = amplitude from the envelope generator, fixed level bits zero.
        // With no envelope shape ever written there is nothing to hear.
        // Observed in the wild ("Onryou Senki" tracks), which render to zero.
        let log = log_with(Chip::Ym2203, &[(0, 0x07, 0x18), (0, 0x0A, 0x10)]);
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn ssg_envelope_mode_with_a_shape_is_audible() {
        let log = log_with(
            Chip::Ym2203,
            &[(0, 0x07, 0x18), (0, 0x0A, 0x10), (0, 0x0C, 0x40), (0, 0x0D, 0x0E)],
        );
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn adpcm_only_is_its_own_class() {
        // OPNA ADPCM-B control writes with nothing else: real music that
        // neither writer can currently represent.
        let log = log_with(Chip::Ym2608, &[(1, 0x00, 0x80), (1, 0x01, 0x00), (1, 0x10, 0x80)]);
        assert_eq!(audibility(&log), Audibility::AdpcmOnly);
        assert!(Audibility::AdpcmOnly.is_silent());
    }

    #[test]
    fn adpcm_with_silent_keyons_is_still_adpcm() {
        // Observed on The Scheme (OPNA): keyon=240 alongside adpcm=726, with no
        // voice ever programmed. Previously reported as NoVoice, hiding the
        // ADPCM content from the census. Both classes drop the track, so this
        // is about the label being truthful.
        let log = log_with(Chip::Ym2608, &[(1, 0x00, 0x80), (0, 0x40, 0x7F), (0, 0x28, 0xF0)]);
        assert_eq!(audibility(&log), Audibility::AdpcmOnly);
    }

    #[test]
    fn adpcm_alongside_fm_is_plain_audible() {
        let log = log_with(Chip::Ym2608, &[(1, 0x00, 0x80), (0, 0x40, 0x20), (0, 0x28, 0xF0)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn opna_fm_ch4_registers_are_not_adpcm() {
        // Port 1 reg 0x40 is FM channel 4's TL, well clear of the 0x00..=0x10
        // ADPCM window.
        let log = log_with(Chip::Ym2608, &[(1, 0x40, 0x7F)]);
        assert_eq!(markers(&log).adpcm, 0);
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn adpcm_range_on_plain_opn_is_not_adpcm() {
        // A YM2203 has no ADPCM-B at all.
        let log = log_with(Chip::Ym2203, &[(1, 0x00, 0x80)]);
        assert_eq!(markers(&log).adpcm, 0);
    }

    #[test]
    fn silent_ssg_level_is_not_tone() {
        let log = log_with(Chip::Ym2203, &[(0, 0x08, 0x00), (0, 0x09, 0x00)]);
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn opna_rhythm_alone_is_audible() {
        let log = log_with(Chip::Ym2608, &[(0, 0x10, 0x01)]);
        assert_eq!(audibility(&log), Audibility::Audible);
    }

    #[test]
    fn opna_rhythm_dump_is_not_a_trigger() {
        // Bit 7 set is the mute/dump form, not a key-on.
        let log = log_with(Chip::Ym2608, &[(0, 0x10, 0xBF)]);
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn rhythm_reg_on_plain_opn_is_not_rhythm() {
        // 0x10 has no rhythm meaning on a YM2203.
        let log = log_with(Chip::Ym2203, &[(0, 0x10, 0x01)]);
        assert_eq!(audibility(&log), Audibility::Dead);
    }

    #[test]
    fn non_opn_chip_falls_back_to_write_presence() {
        let log = log_with(Chip::Ym2151, &[(0, 0x28, 0x00)]);
        assert_eq!(audibility(&log), Audibility::Audible);
        assert_eq!(audibility(&log_with(Chip::Ym2151, &[])), Audibility::Dead);
    }

    /// The live-rip path and the triage path must never disagree about the
    /// same music, or quarantine would fight the ripper.
    #[test]
    fn s98_roundtrip_agrees_with_live_classification() {
        use crate::s98::{read_s98, write_s98, S98Tags};
        let cases = [
            (Chip::Ym2203, vec![(0u8, 0x27u8, 0x30u8), (0, 0x40, 0x7F)], Audibility::Dead),
            (Chip::Ym2203, vec![(0, 0x40, 0x7F), (0, 0x28, 0xF0)], Audibility::NoVoice),
            (Chip::Ym2203, vec![(0, 0x40, 0x20), (0, 0x28, 0xF0)], Audibility::Audible),
            (Chip::Ym2608, vec![(0, 0x10, 0x01)], Audibility::Audible),
            (Chip::Ym2608, vec![(1, 0x40, 0x18), (0, 0x28, 0xF4)], Audibility::Audible),
            (Chip::Ym2608, vec![(1, 0x00, 0x80), (1, 0x10, 0x80)], Audibility::AdpcmOnly),
            (Chip::Ym2203, vec![(0, 0x07, 0xBF), (0, 0x0A, 0x31)], Audibility::Dead),
            (Chip::Ym2203, vec![(0, 0x07, 0xBF), (0, 0x0A, 0x0C), (0, 0x07, 0xBB)], Audibility::Audible),
            (Chip::Ym2203, vec![(0, 0x07, 0x18), (0, 0x0A, 0x10)], Audibility::Dead),
            (Chip::Ym2203, vec![(0, 0x07, 0x18), (0, 0x0A, 0x10), (0, 0x0D, 0x0E)], Audibility::Audible),
        ];
        for (chip, writes, want) in cases {
            let log = log_with(chip, &writes);
            assert_eq!(audibility(&log), want, "live path, {chip:?} {writes:?}");
            let bytes = write_s98(&log, &S98Tags::default()).unwrap();
            let parsed = read_s98(&bytes).unwrap();
            assert_eq!(audibility_s98(&parsed), want, "s98 path, {chip:?} {writes:?}");
            assert_eq!(markers(&log), markers_s98(&parsed), "markers, {chip:?} {writes:?}");
        }
    }

    #[test]
    fn dump_region_excludes_the_tag_block() {
        use crate::s98::{dump_region, write_s98, S98Tags};
        // Same music, different titles: the dump region must match even though
        // the whole files do not. This is what makes duplicate detection work.
        let log = log_with(Chip::Ym2203, &[(0, 0x40, 0x20), (0, 0x28, 0xF0)]);
        let mut a = S98Tags::default();
        a.set("title", "Track One");
        let mut b = S98Tags::default();
        b.set("title", "A Completely Different Title");
        let fa = write_s98(&log, &a).unwrap();
        let fb = write_s98(&log, &b).unwrap();
        assert_ne!(fa, fb, "tag blocks should differ");
        assert_eq!(dump_region(&fa).unwrap(), dump_region(&fb).unwrap());
    }

    #[test]
    fn silence_predicate_covers_both_failure_classes() {
        assert!(Audibility::Dead.is_silent());
        assert!(Audibility::NoVoice.is_silent());
        assert!(!Audibility::Audible.is_silent());
    }
}
