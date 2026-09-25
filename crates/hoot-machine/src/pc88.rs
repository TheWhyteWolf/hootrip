//! PC-8801 harness for hoot `<driver type="opn|opna">pc88</driver>` sets.
//!
//! hoot's protocol (reverse-engineered from the 72-byte PATCH bootstrap in
//! e.g. pc88/4thunit — see project plan):
//!
//! - files with `type="code"` load at `offset` in RAM; CPU resets at 0x0000
//!   (the PATCH), which sets SP/IM and polls hoot's virtual ports;
//! - virtual port 0x00 = command (1 = play, other nonzero = stop; reading
//!   consumes it), port 0x01 = parameter echo, port 0x80 = song number;
//! - on play, the PATCH calls the driver's init entry, reads the song number,
//!   and calls the play entry with HL = `mdata_addr` + 1, expecting the
//!   selected `type="bgm"` file (offset == song number) at `mdata_addr`;
//! - the driver paces itself with OPN timer IRQs (PC-88 sound INT4, IM 2
//!   vector byte 0x08) and/or the 600 Hz RTC interrupt (level 0, vector 0x00,
//!   enabled by `<option name="use_rtc">`).

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use hoot_chips::Opn;
use hoot_log::{Chip, Device, RegWrite, RegisterLog};
use hoot_xml::{parse_num, Game};
use iz80::{Cpu, Machine, Reg8};

/// PC-8801 Z80 clock (hoot: `z80_emulate(4000000*sec)` in mucom88.cpp).
pub const PC88_CPU_HZ: u32 = 4_000_000;
/// YM2203 (OPN) clock (hoot: `m_YM2203->Initialize(3993600)`) — a domain
/// separate from the CPU, so the OPN timer advances in real chip-clock units.
pub const PC88_OPN_CLOCK_HZ: u32 = 3_993_600;
/// OPNA (Sound Board II) clock, declared in logs when extended regs are used.
pub const PC88_OPNA_CLOCK_HZ: u32 = 7_987_200;
/// PC-88 interrupt vector bytes (IM 2 low byte = level × 2), verified
/// empirically: 4thunit's 600 Hz clock ISR accepts vector 0x04, and pmd2g
/// (Juvenilias) installs its VRTC tick at 0x02 and its OPN INT4 handler at
/// 0x08 (levels: 1 = VRTC, 2 = clock, 4 = sound).
const VEC_VRTC: u8 = 0x02;
const VEC_RTC: u8 = 0x04;
const VEC_SOUND: u8 = 0x08;
/// Clock interrupt rate.
const RTC_HZ: u64 = 600;
/// Vertical retrace rate ×1000 (≈56.4 Hz, PC-88 15 kHz timing). TODO:
/// calibrate against a hoot s98 log once the Wine ground-truth rig exists.
const VRTC_MILLIHZ: u64 = 56_400;

pub struct Pc88Bus {
    pub mem: Box<[u8; 0x10000]>,
    pub opn: Opn,
    /// Chip register writes with absolute cycle timestamps.
    pub writes: Vec<RegWrite>,
    /// Current CPU cycle, set by the runner before each instruction.
    pub now: u64,
    /// CPU clock (cycles per second) — the timestamp/time base.
    pub cpu_hz: u64,
    /// hoot virtual command port (0x00): read-and-clear.
    pub cmd: u8,
    /// hoot virtual parameter port (0x01).
    pub param: u8,
    /// hoot virtual song-number port (0x80).
    pub song: u8,
    /// PC-88 sound-board IRQ mask (port 0x32 bit 7 set = OPN timer IRQ inhibited).
    pub sound_irq_masked: bool,
    /// port → (reads, writes) for ports we don't model.
    pub unknown_ports: BTreeMap<u8, (u64, u64)>,
}

impl Pc88Bus {
    fn new(opn_clock_hz: u32, cpu_hz: u64) -> Self {
        Pc88Bus {
            mem: vec![0u8; 0x10000].into_boxed_slice().try_into().unwrap(),
            opn: Opn::new(opn_clock_hz),
            writes: Vec::new(),
            now: 0,
            cpu_hz,
            cmd: 0,
            param: 0,
            song: 0,
            sound_irq_masked: false,
            unknown_ports: BTreeMap::new(),
        }
    }

    /// Synthesized VRTC bit for port 0x40 reads (bit 5): ~1.4 ms active per
    /// 16.7 ms frame, so drivers polling for vblank edges keep making progress.
    /// Uses the CPU clock domain (the timestamp base).
    fn port40(&self, cpu_hz: u64) -> u8 {
        let frame = cpu_hz / 60;
        let vblank = frame / 12;
        if self.now % frame < vblank {
            0x20
        } else {
            0x00
        }
    }
}

impl Machine for Pc88Bus {
    fn peek(&mut self, address: u16) -> u8 {
        self.mem[address as usize]
    }

    fn poke(&mut self, address: u16, value: u8) {
        self.mem[address as usize] = value;
    }

    fn port_in(&mut self, address: u16) -> u8 {
        match (address & 0xFF) as u8 {
            0x00 => {
                let v = self.cmd;
                self.cmd = 0;
                v
            }
            0x01 => self.param,
            0x80 => self.song,
            0x40 => self.port40(self.cpu_hz),
            0x44 => self.opn.read_status(),
            0x45 => self.opn.read_data(),
            0x46 => self.opn.read_status1(),
            0x47 => self.opn.read_data1(),
            p => {
                self.unknown_ports.entry(p).or_default().0 += 1;
                0xFF
            }
        }
    }

    fn port_out(&mut self, address: u16, value: u8) {
        match (address & 0xFF) as u8 {
            0x00 => self.cmd = value,
            0x01 => self.param = value,
            // Sound-board IRQ mask (hoot mucom88: `ioport[0x32] & 0x80`).
            0x32 => self.sound_irq_masked = value & 0x80 != 0,
            0x44 => self.opn.write_addr(value),
            0x46 => self.opn.write_addr1(value),
            0x45 | 0x47 => {
                if address & 0xFF == 0x45 {
                    self.opn.write_data(value);
                } else {
                    self.opn.write_data1(value);
                }
                if let Some((port, addr, data)) = self.opn.last_write.take() {
                    self.writes.push(RegWrite {
                        t: self.now,
                        dev: 0,
                        port,
                        addr,
                        data,
                    });
                }
            }
            // OPN interrupt acknowledge (hoot mucom88: `z80_lower_IRQ()` on 0xe4).
            0xE4 => self.opn.ack_irq(),
            p => {
                self.unknown_ports.entry(p).or_default().1 += 1;
            }
        }
    }
}

pub struct RipOptions {
    /// Emulated seconds to run after triggering the song.
    pub seconds: f64,
    /// Wall-clock cap (seconds) for this one title. A driver that never reaches
    /// its poll loop executes the whole emulated budget instruction by
    /// instruction, which is slow; this bounds it so a broad sweep can't stall
    /// on a non-working set. `None` = no wall-clock cap (normal rips).
    pub deadline_secs: Option<f64>,
    /// Emulated seconds allowed for the bootstrap to reach its poll loop.
    pub boot_seconds: f64,
    /// CPU clock multiplier (hoot `clockmul`; PC-88 default 1).
    pub clockmul: u32,
}

impl Default for RipOptions {
    fn default() -> Self {
        RipOptions { seconds: 120.0, deadline_secs: None, boot_seconds: 0.5, clockmul: 1 }
    }
}

pub struct RipOutcome {
    pub log: RegisterLog,
    /// Unmodelled port traffic, for diagnostics.
    pub unknown_ports: BTreeMap<u8, (u64, u64)>,
    /// Interrupts delivered (rtc, opn).
    pub irqs: (u64, u64),
    /// IM 2 vector-table entries for levels 0-7 at end of run (diagnostics).
    pub vectors: [u16; 8],
    /// True if the run hit `RipOptions::deadline_secs` (a spinning driver).
    /// The log is a partial capture when this is set.
    pub timed_out: bool,
}

/// Rip one title from a pc88 game: load, boot, trigger, run, log.
pub fn rip_title(
    game: &Game,
    set_dir: &std::path::Path,
    title_code: u64,
    opts: &RipOptions,
) -> Result<RipOutcome> {
    let romlist = game.romlist.as_ref().context("game has no romlist")?;
    // clockmul scales the CPU only; the OPN keeps its real chip clock.
    let cpu_hz = (PC88_CPU_HZ as u64) * opts.clockmul.max(1) as u64;
    let deadline = opts
        .deadline_secs
        .map(|s| std::time::Instant::now() + std::time::Duration::from_secs_f64(s));

    let mut bus = Pc88Bus::new(PC88_OPN_CLOCK_HZ, cpu_hz);
    // OPNA sets carry a YM2608 Sound Board II; mark the chip so register-0xFF
    // reads return the OPNA identity byte that drivers like MUCOM88 probe for.
    bus.opn.set_opna(game.driver.kind.as_deref() == Some("opna"));
    let mut cpu = Cpu::new_z80();

    // --- Load code images -------------------------------------------------
    for rom in &romlist.roms {
        if rom.kind != "code" {
            continue;
        }
        let offset = rom.offset.unwrap_or(0);
        if offset < 0 {
            bail!("negative code offset for {}", rom.name);
        }
        let data = read_set_file(set_dir, &rom.name)?;
        let start = offset as usize;
        if start + data.len() > 0x10000 {
            bail!("{} does not fit at {:#x}", rom.name, start);
        }
        bus.mem[start..start + data.len()].copy_from_slice(&data);
    }

    // --- Load the selected bgm file at mdata_addr -------------------------
    let song = (title_code & 0xFF) as u8;
    let mdata_addr = game
        .options
        .iter()
        .find(|o| o.name == "mdata_addr")
        .and_then(|o| parse_num(&o.value));
    // Load the selected song's bgm bank at mdata_addr when the set declares
    // one. Sets without mdata_addr fall into two families: (a) placeholder
    // "DUMMY" bgm entries whose music actually lives in a code rom — nothing
    // to load; (b) drivers that copy the bank on demand when the song number
    // is written to port 0x00 (hoot mucom88: memcpy to ram[0x5d:0x5c]).
    // Family (b) is not yet emulated (needs the per-driver destination); such
    // sets currently produce no writes rather than erroring. TODO: port-0x00
    // bank-copy handler. See sweep findings in the project plan.
    // MUCOM88 (v1.5+) uses a different data/trigger model than the classic
    // PATCH: its compiled song data (`bgm`) is read by the driver's WORKINIT at
    // MU_TOP (0xC205), and its 127-byte patch triggers play with `CALL 0xEEA7`
    // — a routine real hoot injects but that is otherwise unmapped RAM here.
    let is_mucom =
        game.driver_alias.as_ref().is_some_and(|a| a.label.eq_ignore_ascii_case("Mucom"));
    const MUCOM_DATA_ADDR: i64 = 0xC205; // MU_TOP: MUSICNUM(0xC200)+5
    let bgm_addr = if is_mucom { Some(MUCOM_DATA_ADDR) } else { mdata_addr };
    if let Some(bgm) = romlist
        .roms
        .iter()
        .find(|r| r.kind == "bgm" && r.offset == Some(song as i64))
    {
        if let Some(addr) = bgm_addr {
            let data = read_set_file(set_dir, &bgm.name)?;
            let start = addr as usize;
            if start + data.len() > 0x10000 {
                bail!("{} does not fit at {:#x}", bgm.name, start);
            }
            bus.mem[start..start + data.len()].copy_from_slice(&data);
        }
    }

    // Replicate hoot's injected MUCOM play trigger: stub 0xEEA7 as `XOR A;
    // JP MSTART` (music+0 = 0xB000). The patch calls it with the song number,
    // but each hoot MUCOM `bgm` file is a single-song MUB, so play index 0.
    if is_mucom {
        bus.mem[0xEEA7] = 0xAF; // XOR A
        bus.mem[0xEEA8] = 0xC3; // JP nn
        bus.mem[0xEEA9] = 0x00;
        bus.mem[0xEEAA] = 0xB0; // -> 0xB000 (JP MSTART)
    }

    let opt_flag = |name: &str| {
        game.options
            .iter()
            .any(|o| o.name == name && parse_num(&o.value).unwrap_or(0) != 0)
    };
    let use_rtc = opt_flag("use_rtc");
    let use_vrtc = opt_flag("use_vrtc");

    // hoot's `init_pc` option is the driver's entry point. Most sets omit it
    // (or set 0) and load their PATCH at 0x0000, which the Z80 reset already
    // enters; ~130 pc88 sets — including the whole MUCOM88/muco family — load
    // the PATCH higher (0x8000, 0xf000, …) and rely on init_pc to jump there.
    // Without it the CPU NOP-slides from 0x0000 and never reaches the driver.
    let init_pc = game
        .options
        .iter()
        .find(|o| o.name == "init_pc")
        .and_then(|o| parse_num(&o.value))
        .unwrap_or(0);
    cpu.registers().set_pc(init_pc as u16);

    // --- Boot: let the PATCH reach its poll loop --------------------------
    let boot_cycles = (opts.boot_seconds * cpu_hz as f64) as u64;
    let mut runner = Runner {
        cpu: &mut cpu,
        bus: &mut bus,
        use_rtc,
        use_vrtc,
        cpu_hz,
        opn_hz: PC88_OPN_CLOCK_HZ as u64,
        rtc_period: cpu_hz / RTC_HZ,
        next_rtc: cpu_hz / RTC_HZ,
        vrtc_period: cpu_hz * 1000 / VRTC_MILLIHZ,
        next_vrtc: cpu_hz * 1000 / VRTC_MILLIHZ,
        opn_residual: 0,
        irqs: (0, 0),
        deadline,
        timed_out: false,
    };
    runner.run_until(boot_cycles);

    // --- Trigger playback ---------------------------------------------------
    // Song number is exposed on port 0x80 (song) for the classic PATCH and on
    // port 0x01 (param) for the MUCOM88 patch, which reads IN(0x01) after
    // seeing cmd==1; set both so either bootstrap picks it up.
    runner.bus.song = song;
    runner.bus.param = song;
    runner.bus.cmd = 1;
    let t0 = runner.cpu.cycle_count();
    let end = t0 + (opts.seconds * cpu_hz as f64) as u64;
    runner.run_until(end);
    let irqs = runner.irqs;
    // A deadline abort leaves the CPU short of `end`; log only what was reached
    // so the capture's end_t matches the writes it actually contains.
    let timed_out = runner.timed_out;
    let end = if timed_out { runner.cpu.cycle_count().max(t0) } else { end };

    // --- Collect the log ----------------------------------------------------
    // Declare YM2608 when the driver touched the extended bank; at these
    // clocks the two chips' F-number and timer semantics coincide, so the
    // register stream needs no translation either way.
    let device = if bus.opn.port1_used {
        Device { chip: Chip::Ym2608, clock_hz: PC88_OPNA_CLOCK_HZ }
    } else {
        Device { chip: Chip::Ym2203, clock_hz: PC88_OPN_CLOCK_HZ }
    };
    let mut log = RegisterLog::new(cpu_hz, vec![device]);
    for w in bus.writes.iter().filter(|w| w.t >= t0) {
        log.push(RegWrite { t: w.t - t0, ..*w });
    }
    log.end_t = end - t0;

    let i_reg = cpu.immutable_registers().get8(Reg8::I);
    let mut vectors = [0u16; 8];
    for (level, v) in vectors.iter_mut().enumerate() {
        let table = ((i_reg as u16) << 8) | (level as u16 * 2);
        *v = bus.peek16(table);
    }

    Ok(RipOutcome { log, unknown_ports: bus.unknown_ports.clone(), irqs, vectors, timed_out })
}

struct Runner<'a> {
    cpu: &'a mut Cpu,
    bus: &'a mut Pc88Bus,
    use_rtc: bool,
    use_vrtc: bool,
    cpu_hz: u64,
    opn_hz: u64,
    rtc_period: u64,
    next_rtc: u64,
    vrtc_period: u64,
    next_vrtc: u64,
    /// Fractional CPU-cycle accumulator for OPN-clock conversion (units of cpu_hz).
    opn_residual: u64,
    irqs: (u64, u64),
    /// Wall-clock cutoff for the whole title, or `None` for no cap.
    deadline: Option<std::time::Instant>,
    /// True if a `run_until` returned early because it hit `deadline`.
    timed_out: bool,
}

/// Instructions between wall-clock checks in [`Runner::run_until`].
/// `Instant::now()` is a syscall-ish cost, so poll it rarely; at ~4 MHz this is
/// still a sub-millisecond granularity on the guard.
const WALL_POLL_INSNS: u32 = 65_536;

impl Runner<'_> {
    /// IM 2 vector-table entry for a data-bus byte, at the CPU's current I.
    fn im2_target(&mut self, vector: u8) -> u16 {
        let i = self.cpu.immutable_registers().get8(Reg8::I);
        self.bus.peek16(((i as u16) << 8) | vector as u16)
    }

    fn run_until(&mut self, end_cycle: u64) {
        let mut poll = 0u32;
        while self.cpu.cycle_count() < end_cycle {
            // Spin-guard: bail out of a driver that will never finish its
            // emulated budget in reasonable wall time. Checked every
            // WALL_POLL_INSNS instructions so the normal path pays ~nothing.
            if let Some(dl) = self.deadline {
                poll += 1;
                if poll >= WALL_POLL_INSNS {
                    poll = 0;
                    if std::time::Instant::now() >= dl {
                        self.timed_out = true;
                        return;
                    }
                }
            }
            let now = self.cpu.cycle_count();
            self.bus.now = now;

            // Interrupt delivery, priority order: VRTC (1), clock (2), sound (3).
            let mut vrtc_due = self.use_vrtc && now >= self.next_vrtc;
            let mut rtc_due = self.use_rtc && now >= self.next_rtc;
            // OPN timer IRQ is gated by the sound-board mask (port 0x32 bit 7).
            let mut opn_due = self.bus.opn.irq() && !self.bus.sound_irq_masked;
            let (iff1, im) = self.cpu.immutable_registers().get_interrupt_mode();
            if iff1 && im == 2 {
                // Never vector through an uninstalled IM 2 table entry — a
                // zero target would "jump to 0" and wreck the driver. hoot
                // sets use_rtc even for drivers that never install a clock
                // ISR (it also covers the µPD1990 calendar device), so this
                // gate is what actually decides which sources are live.
                vrtc_due &= self.im2_target(VEC_VRTC) != 0;
                rtc_due &= self.im2_target(VEC_RTC) != 0;
                opn_due &= self.im2_target(VEC_SOUND) != 0;
            }
            if iff1 && (vrtc_due || rtc_due || opn_due) {
                let vector = if vrtc_due {
                    VEC_VRTC
                } else if rtc_due {
                    VEC_RTC
                } else {
                    VEC_SOUND
                };
                self.cpu.set_interrupt_vector(vector);
                self.cpu.signal_interrupt(true);
                self.cpu.execute_instruction(self.bus);
                self.cpu.signal_interrupt(false);
                if self.cpu.take_interrupt_accepted() {
                    match vector {
                        VEC_VRTC => {
                            self.next_vrtc += self.vrtc_period;
                            self.irqs.0 += 1;
                        }
                        VEC_RTC => {
                            self.next_rtc += self.rtc_period;
                            self.irqs.0 += 1;
                        }
                        _ => self.irqs.1 += 1,
                    }
                }
            } else {
                // Nobody listening: don't let missed ticks pile up.
                if rtc_due {
                    self.next_rtc = now + self.rtc_period;
                }
                if vrtc_due {
                    self.next_vrtc = now + self.vrtc_period;
                }
                self.cpu.execute_instruction(self.bus);
            }

            // Advance the OPN in its own clock domain: elapsed CPU cycles
            // converted to real OPN clocks (opn_hz / cpu_hz), drift-free.
            let after = self.cpu.cycle_count();
            self.opn_residual += (after - now) * self.opn_hz;
            let opn_ticks = self.opn_residual / self.cpu_hz;
            self.opn_residual %= self.cpu_hz;
            self.bus.opn.tick(opn_ticks);
        }
    }
}

fn read_set_file(dir: &std::path::Path, name: &str) -> Result<Vec<u8>> {
    // Set files are case-inconsistent between XML and disk; try exact first.
    let direct = dir.join(name);
    if direct.is_file() {
        return std::fs::read(&direct).with_context(|| format!("reading {}", direct.display()));
    }
    let lower = name.to_lowercase();
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().to_lowercase() == lower {
            return std::fs::read(entry.path())
                .with_context(|| format!("reading {}", entry.path().display()));
        }
    }
    bail!("file {name:?} not found in {}", dir.display())
}
