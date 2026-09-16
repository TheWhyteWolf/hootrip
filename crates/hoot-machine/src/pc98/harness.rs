//! The `pc98dos` rip harness: assemble the NP2 i286c core + [`MiniDos`] +
//! [`Pc98Io`], re-host a set's shell-command chain, pace the resident sound
//! driver with its timer IRQ, and log OPN/OPNA register writes.
//!
//! # Model (from the set XML, see [`super`] and the project plan)
//! A `pc98dos` set is a virtual DOS working directory of loose files plus a
//! short list of shell commands:
//! - `<rom type="file" offset="-1">PROG.COM</rom>` — an executable to run.
//! - `<rom type="file" offset="0xNN">SONG.M</rom>` — a song data file; `offset`
//!   is the title code that selects it.
//! - `<rom type="shell">pmd #/k</rom>` — a command line; `#` is replaced by the
//!   selected song's file name. Commands run in listed order; the last is
//!   usually a resident driver stub whose timer ISR plays the music.
//! - `<rom type="binary" offset="0xNN">DEC</rom>` — patch one byte of the most
//!   recently listed file.
//!
//! # Pacing (unknowns resolved by observation)
//! Which IVT vector the driver installs its timer ISR on, and whether it plays
//! in the foreground or after going resident (TSR), are not documented. The
//! engine therefore *auto-detects* the sound vector by scanning the IVT for an
//! entry the program hooked in the PC-98 hardware-IRQ range (0x08..=0x1F,
//! excluding the driver `funcvect` API), and reports rich diagnostics so the
//! trigger/capture strategy can be refined on evidence — the same empirical
//! approach the PC-88 harness used.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use hoot_cpu::{flag, np2::Np2Cpu, Reg16, Stop, X86Cpu};
use hoot_log::{Chip, Device, RegWrite, RegisterLog};
use hoot_xml::{parse_num, Game, RomList};

use super::dos::{
    BiosTimerReq, ExecResult, MiniDos, ProgKind, ARENA_END, CALL_RET_OFF, TRAMP_SEG,
};
use super::io::Pc98Io;

/// Nominal PC-98 CPU clock (80286/V30 class, ~8 MHz). The OPN is paced in its
/// own chip-clock domain, so timing fidelity does not depend on this exactly;
/// it is the log timestamp base and the run-budget unit.
pub const PC98_CPU_HZ: u32 = 8_000_000;
/// Where the PC-98 sound BIOS records the software interrupt it serves, inside
/// the board's BIOS ROM. Drivers read this byte to find the sound BIOS and to
/// confirm a board is fitted at all. See `Pc98RipOptions::dummy_sndrom`.
const PC98_SNDROM_INT_OFF: usize = 0xCEE04;
/// The interrupt the PC-98 sound BIOS serves.
const PC98_SNDROM_INT: u8 = 0xD2;

/// One PC-98 timer-BIOS tick in CPU cycles. The BIOS timer runs at 100 Hz, so a
/// tick is 10 ms; see `MiniDos::int1c` for what the guest does with it and why
/// only the order of magnitude matters.
const PC98_BIOS_TICK_CPU: u64 = PC98_CPU_HZ as u64 / 100;
/// PC-9801-26(K) OPN (YM2203) clock.
pub const PC98_OPN_CLOCK_HZ: u32 = 3_993_600;
/// PC-9801-86 OPNA (YM2608) clock.
pub const PC98_OPNA_CLOCK_HZ: u32 = 7_987_200;

/// Segment of a one-byte `HLT` idle stub the harness parks the CPU on during
/// the capture phase. Sits in the free gap between the trampoline table (linear
/// 0x800..0x9FF) and the arena (0x10000+), so its stack cannot stomp either.
const IDLE_SEG: u16 = 0x00A0;
/// CPU cycles to advance the OPN clock by when the CPU idles on `HLT` waiting
/// for its next timer IRQ (~25 µs at 8 MHz — fine vs a ms-scale timer period).
const IDLE_QUANTUM_CPU: u64 = 200;
/// PC-98 IRQ0 (8253 PIT channel 0) real-mode vector. PC-98 sound drivers
/// (PMD, etc.) install their tempo ISR here and pace on the system timer.
const PC98_TIMER_VEC: u8 = 0x08;
/// PC-98 IRQ3 real-mode vector: the sound board's OPN(A) chip `/IRQ` line. The
/// OPN-timer-paced drivers install their sequencer ISR here.
const PC98_OPN_IRQ_VEC: u8 = 0x0B;
/// PC-98 IRQ2 real-mode vector: the CRT vertical-sync (VSYNC) interrupt. Frame-
/// paced sound engines (A-Train's ARTDI, VSYNCMAN) hook it and advance one
/// sequencer tick per vertical retrace.
const PC98_VSYNC_VEC: u8 = 0x0A;
/// PC-98 IRQ12 real-mode vector. The sound board's interrupt jumper can route
/// the OPN(A) `/IRQ` here instead of IRQ3, and the board setup in this harness
/// forces the jumper bits that select it (see the INT 0x14 segment word at
/// 0000:0052), so a driver hooking 0x14 is naming its sound ISR just as
/// explicitly as one hooking 0x0B.
const PC98_SOUND_IRQ12_VEC: u8 = 0x14;
/// Device-driver INIT scratch, all in the free low RAM between the trampoline
/// table (ends linear 0x800) and the arena (0x10000), disjoint from the capture
/// idle stub at [`IDLE_SEG`]. Live only for the duration of the INIT far calls.
/// The INIT request header (26 bytes) sits at [`DEV_REQ_SEG`]:0, its
/// CR-terminated arg string at [`DEV_ARG_SEG`]:0, and the far-call runs on a
/// private stack at [`DEV_STACK_SEG`]:[`DEV_STACK_SP`].
const DEV_REQ_SEG: u16 = 0x00B0; // linear 0xB00
const DEV_ARG_SEG: u16 = 0x00B2; // linear 0xB20
const DEV_STACK_SEG: u16 = 0x00C0; // linear 0xC00
const DEV_STACK_SP: u16 = 0x0F00; // ~3.8 KB, top linear 0x1B00 (below the arena)

/// Run batch. Kept well below a timer period (≳1000 cycles) so no PIT wrap is
/// ever skipped between interrupt-delivery checks — the tempo calibration that
/// counts ticks depends on delivering every one.
const SETUP_BURST: u32 = 128;
/// Capture run batch (also fine-grained for register-write timestamps).
const CAPTURE_BURST: u32 = 128;

/// Options controlling a `pc98dos` rip.
pub struct Pc98RipOptions {
    /// Emulated seconds to record once the driver is playing.
    pub seconds: f64,
    /// Emulated seconds each shell command may run before we move on.
    pub setup_seconds: f64,
    /// hoot `clockmul` (reported; the OPN is self-paced so it is not applied to
    /// the CPU clock — noted as a calibration TODO).
    pub clockmul: u32,
    /// Driver `funcvect` INT number, excluded from sound-vector auto-detect.
    pub funcvect: Option<u8>,
    /// Force the timer/sound IRQ vector instead of auto-detecting it.
    pub sound_vector: Option<u8>,
    /// Wall-clock cap (seconds) for the whole rip. A set whose driver spins
    /// (never idles to HLT) executes its full emulated budget instruction by
    /// instruction, which is slow; this bounds it so a broad sweep can't stall
    /// on a non-working set. `None` = no wall-clock cap (normal rips).
    pub deadline_secs: Option<f64>,
    /// Override the OPN(A) clock (Hz). `None` uses the per-kind default (OPN
    /// 3.9936 MHz / OPNA 7.9872 MHz). This paces the driver's chip timer (tempo)
    /// and is the declared device clock — used to A/B ambiguous board clocks.
    pub opn_clock_hz: Option<u32>,
    /// Rip the FM soundtrack of a Vermouth/TGLFMP2 MIDI set: bind the `.MFM`
    /// sibling of each song instead of its `.MGS`/`.MG2` MIDI data, and strip the
    /// `-m`/`-M` MIDI switch from the `fmp*` shell so FMP3 installs as an FM
    /// driver. These sets play their melody out as MIDI (which we do not capture);
    /// the original FM soundtrack ships alongside as `.MFM` and is the only FM
    /// copy of this music in the archive. No-op for sets without a `.MFM` sibling.
    pub fm_variant: bool,
    /// hoot `dummysndrom`: present the PC-98 sound BIOS as installed.
    ///
    /// A real PC-9801-26K/86 board carries a BIOS ROM, and software that drives
    /// the board through it first asks the ROM which software interrupt it lives
    /// on. hoot maps a stand-in for sets that ask; without one, FUGA System's
    /// OPNDRV 1.23 reads zero, decides the machine has no sound board and keeps
    /// only an 80-byte stub resident, so every note is lost.
    ///
    /// We model the single field we have direct evidence a driver reads — the
    /// interrupt number, [`PC98_SNDROM_INT_OFF`] — rather than inventing ROM
    /// contents. A set that probes something else will show up as still silent,
    /// which is the honest outcome.
    pub dummy_sndrom: bool,
}

impl Default for Pc98RipOptions {
    fn default() -> Self {
        Pc98RipOptions {
            seconds: 60.0,
            setup_seconds: 3.0,
            clockmul: 1,
            funcvect: None,
            sound_vector: None,
            deadline_secs: None,
            opn_clock_hz: None,
            fm_variant: false,
            dummy_sndrom: false,
        }
    }
}

/// What became of one shell command in the chain.
#[derive(Clone, Debug)]
pub enum StepResult {
    /// Program terminated with the given code (INT 20h / INT 21h AH=4Ch).
    Terminated(u8),
    /// Program went resident (TSR); its memory and ISRs persist.
    Resident,
    /// A funcvect glue stub finished installing (wrote its ready handshake) and
    /// is idling, waiting for the harness to invoke its INT to play.
    StubReady,
    /// Program ran the whole setup budget without finishing (e.g. a foreground
    /// player, or a hang) — diagnostic, not necessarily an error.
    RanToBudget,
    /// The command could not be loaded/run.
    Error(String),
}

/// Why [`Engine::pump`] stopped.
enum PumpEnd {
    /// The program terminated or went resident.
    Ended(ExecResult),
    /// A funcvect glue stub signalled ready and is idling.
    StubReady,
    /// The cycle budget was exhausted.
    Budget,
}

/// One shell command and its outcome.
#[derive(Clone, Debug)]
pub struct ShellStep {
    pub cmd: String,
    pub result: StepResult,
    pub cycles: u64,
}

/// Result of a `pc98dos` rip, with diagnostics for the still-empirical bits.
pub struct Pc98RipOutcome {
    /// The register log (empty if nothing was captured).
    pub log: RegisterLog,
    /// Each shell command and how it ended.
    pub shell: Vec<ShellStep>,
    /// IVT entries the program(s) hooked (vector -> seg:off), i.e. no longer
    /// pointing at the harness trampoline. Reveals the timer ISR + funcvect.
    pub hooked_vectors: BTreeMap<u8, (u16, u16)>,
    /// Vectors installed via INT 21h AH=25h specifically (subset of hooked).
    pub installed_vectors: BTreeMap<u8, (u16, u16)>,
    /// INT/AH pairs the mini-DOS does not implement yet, with call counts.
    pub unimpl: BTreeMap<(u8, u8), u64>,
    /// Console output the programs produced (lossy UTF-8).
    pub console: String,
    /// Unmodelled port traffic: port -> (reads, writes).
    pub unknown_ports: BTreeMap<u16, (u64, u64)>,
    /// The sound/timer IRQ vector used to pace capture (auto or forced).
    pub sound_vector: Option<u8>,
    /// OPN timer IRQs delivered during capture.
    pub irqs: u64,
    /// OPN data writes seen during the whole run (setup + capture).
    pub total_fm_writes: u64,
    /// OPN data writes captured (the log's length).
    pub captured_writes: usize,
    /// Whether the OPN timer ever asserted its IRQ line (driver uses it).
    pub opn_timer_used: bool,
    /// Whether the PIT (system timer, IRQ0 → INT 08h) drove the driver.
    pub pit_timer_used: bool,
    /// OPN IRQ line state and Timer A/B state at capture end, for diagnosing a
    /// stalled timer: (irq, timer_a(run,en,counter,period,flag), timer_b(...)).
    pub opn_end_state: (bool, (bool, bool, u32, u32, bool), (bool, bool, u32, u32, bool)),
    /// Diagnostics: OPN IRQs pending-but-undelivered because IF was clear / no
    /// handler was hooked.
    pub dbg_if_clear: u64,
    pub dbg_no_vec: u64,
    /// MCB chain after the shell setup: (mcb_seg, owner_psp, size_paras).
    pub mcb_chain: Vec<(u16, u16, u16)>,
    /// True if the run hit its wall-clock deadline (a spinning, non-working set).
    pub timed_out: bool,
}

fn lin(seg: u16, off: u16) -> usize {
    ((seg as usize) << 4) + off as usize
}

/// Write a little-endian word into the flat image at a linear address.
fn wr16(mem: &mut [u8], at: usize, v: u16) {
    mem[at] = v as u8;
    mem[at + 1] = (v >> 8) as u8;
}

/// Read a little-endian word from the flat image at a linear address.
fn rd16(mem: &[u8], at: usize) -> u16 {
    mem[at] as u16 | ((mem[at + 1] as u16) << 8)
}

/// The strategy for pacing the CPU and delivering the timer IRQ.
struct Engine<'a> {
    cpu: &'a mut dyn X86Cpu,
    dos: &'a mut MiniDos,
    io: &'a mut Pc98Io,
    cpu_hz: u64,
    opn_hz: u64,
    /// Fractional CPU-cycle accumulator for OPN-clock conversion (units of cpu_hz).
    opn_residual: u64,
    /// Cumulative CPU cycles — the log timestamp base.
    cycle: u64,
    /// Forced sound vector, else auto-detected from the IVT each delivery.
    forced_vec: Option<u8>,
    /// funcvect to exclude from auto-detect.
    funcvect: Option<u8>,
    irqs: u64,
    opn_timer_used: bool,
    /// Whether the PIT (system timer) ever asserted IRQ0 while unmasked.
    pit_timer_used: bool,
    /// Diagnostics: times an OPN IRQ was pending but not delivered because
    /// interrupts were disabled (IF clear), no handler was hooked, or it was
    /// masked at the PIC (driver in its own ISR).
    dbg_if_clear: u64,
    dbg_no_vec: u64,
    dbg_masked: u64,
    /// The PC-98 timer-BIOS one-shot in flight: routine segment, offset, and the
    /// cycle at which it comes due (see `MiniDos::int1c`).
    bios_timer: Option<(u16, u16, u64)>,
    /// PIC in-service latch for the OPN interrupt: set when we deliver it, cleared
    /// when the driver's ISR writes an EOI. While set, a further OPN IRQ is held
    /// off even if a new edge is pending and IF is enabled — this is the 8259's
    /// "one ISR at a time until EOI" behavior, and it stops a fast Timer A from
    /// re-entering a self-STI-ing ISR (FMP3) so the sequencer's Timer B overflow
    /// is actually observed instead of being reset by a nested entry first.
    opn_in_service: bool,
    /// Optional wall-clock cap; the pump bails out when reached.
    deadline: Option<std::time::Instant>,
    /// True if the last pump stopped because it hit `deadline`.
    timed_out: bool,
}

impl Engine<'_> {
    /// Advance the OPN and PIT by the clocks equivalent to `cpu_cycles`,
    /// drift-free (each keeps its own residual).
    fn advance_timers(&mut self, cpu_cycles: u64) {
        self.opn_residual += cpu_cycles * self.opn_hz;
        let ticks = self.opn_residual / self.cpu_hz;
        self.opn_residual %= self.cpu_hz;
        if ticks > 0 {
            self.io.opn.tick(ticks);
            if self.io.opn.irq() {
                self.opn_timer_used = true;
            }
        }
        self.io.pit.tick(cpu_cycles, self.cpu_hz);
        if self.io.pit.irq_pending && self.io.irq0_unmasked() {
            self.pit_timer_used = true;
        }
        self.io.tick_vsync(cpu_cycles, self.cpu_hz);
    }

    /// The IVT entry for `vec` as (segment, offset).
    fn ivt(&self, vec: u8) -> (u16, u16) {
        let mem = self.cpu.mem_ref();
        let b = vec as usize * 4;
        let off = mem[b] as u16 | ((mem[b + 1] as u16) << 8);
        let seg = mem[b + 2] as u16 | ((mem[b + 3] as u16) << 8);
        (seg, off)
    }

    /// True if `vec` is hooked (points somewhere other than the trampoline).
    fn hooked(&self, vec: u8) -> bool {
        let (seg, _) = self.ivt(vec);
        seg != TRAMP_SEG && seg != 0
    }

    /// The vector the OPN timer IRQ is delivered on: the forced one, else the
    /// lowest hooked vector in the PC-98 hardware-IRQ range (excluding funcvect
    /// and the PIT's own INT 08h).
    fn opn_sound_vec(&self) -> Option<u8> {
        if self.forced_vec.is_some() {
            return self.forced_vec;
        }
        // The PC-98 sound board's OPN(A) /IRQ is wired to IRQ3 → INT 0x0B, where
        // the paced drivers (PMD, FMP3, MMD2, …) put their sequencer ISR, or to
        // IRQ12 → INT 0x14 when the board's interrupt jumper selects it. Both
        // are the driver naming its sound ISR outright, so prefer either over
        // the positional fallback below.
        //
        // The fallback -- lowest hooked hardware vector -- is a guess, and it
        // guesses wrong whenever a driver hooks something lower for an unrelated
        // purpose. `music_98` is the case that exposed it: the driver puts its
        // sequencer at 0x14 but also hooks 0x0A, so every OPN timer IRQ was
        // delivered to 0x0A, landed on the DOS trampoline with no handler, and
        // the music never advanced -- 259 ticks, three key-ons, then silence.
        for cand in [PC98_OPN_IRQ_VEC, PC98_SOUND_IRQ12_VEC] {
            if Some(cand) != self.funcvect && self.hooked(cand) {
                return Some(cand);
            }
        }
        for v in 0x08u8..=0x1F {
            if Some(v) == self.funcvect || v == PC98_TIMER_VEC {
                continue;
            }
            if self.hooked(v) {
                return Some(v);
            }
        }
        None
    }

    /// True if interrupts are currently enabled (IF set).
    fn irqs_enabled(&self) -> bool {
        self.cpu.reg16(Reg16::Flags) & flag::IF != 0
    }

    /// Deliver a pending timer IRQ if interrupts are enabled and a handler is
    /// installed. Prefers the PIT (IRQ0 → INT 08h, the PC-98 system timer that
    /// paces most drivers); falls back to the OPN chip timer for drivers that
    /// use it. Returns true if an interrupt was delivered.
    fn deliver_timer_irq(&mut self) -> bool {
        // The driver's ISR acknowledges the PIC with an EOI when it finishes;
        // that lifts the in-service latch so the next timer IRQ can be delivered.
        if self.io.take_eoi() {
            self.opn_in_service = false;
        }
        // PC-98 timer BIOS (INT 1Ch). The guest posts arm/cancel as a DOS-level
        // call, so pick the request up here and turn an arm into a cycle
        // deadline — before the IF gate, since neither depends on interrupt
        // state.
        match self.dos.bios_timer.take() {
            Some(BiosTimerReq::Arm { seg, off, ticks }) => {
                let due = self.cycle + (ticks.max(1) as u64) * PC98_BIOS_TICK_CPU;
                self.bios_timer = Some((seg, off, due));
            }
            Some(BiosTimerReq::Cancel) => self.bios_timer = None,
            None => {}
        }
        if !self.irqs_enabled() {
            if self.io.opn.peek_irq_edge() {
                self.dbg_if_clear += 1;
            }
            return false;
        }
        if let Some((seg, off, due)) = self.bios_timer {
            if self.cycle >= due {
                self.bios_timer = None;
                enter_far(&mut *self.cpu, seg, off);
                return true;
            }
        }
        // PIT / IRQ0 → INT 08h.
        if self.io.pit.irq_pending && self.io.irq0_unmasked() && self.hooked(PC98_TIMER_VEC) {
            self.io.pit.ack();
            self.cpu.interrupt(PC98_TIMER_VEC);
            self.irqs += 1;
            return true;
        }
        // VSYNC / IRQ2 → INT 0Ah: frame-paced engines (A-Train's ARTDI) calibrate
        // on and sequence off the vertical retrace. Deliver only when a driver has
        // both hooked INT 0Ah AND unmasked IRQ2 at the master PIC (bit 2) — sound
        // drivers that pace on IRQ0/IRQ3 leave IRQ2 masked and are unaffected. The
        // ISR EOIs and re-arms via `out 0x64`; delivery clears the latch and the
        // IF-off during the ISR prevents re-entry, so no in-service latch is needed.
        if self.io.vsync_pending && self.io.pic_mask & 0x04 == 0 && self.hooked(PC98_VSYNC_VEC) {
            self.io.vsync_pending = false;
            self.cpu.interrupt(PC98_VSYNC_VEC);
            self.irqs += 1;
            return true;
        }
        // OPN chip timer: deliver once per latched rising edge of the IRQ line
        // (see `Opn::note_irq_edge`). Edge- rather than level-triggering is what a
        // PC-98 PIC does, and it is essential: a driver whose ISR re-enables
        // interrupts (STI) *before* resetting the timer flag — e.g. FMP3 — would,
        // under level delivery, be re-entered every instruction (an IRQ storm
        // that starves its own sequencer). We do NOT clear the chip flag here; the
        // ISR reads the status to confirm which timer overflowed and resets it via
        // OPN reg 0x27 (which re-arms the edge for the next overflow).
        if self.io.opn.peek_irq_edge() {
            // The previous ISR has not EOI'd yet — hold the edge pending so we
            // deliver exactly one ISR per acknowledgement (no nested re-entry).
            if self.opn_in_service {
                return false;
            }
            if let Some(v) = self.opn_sound_vec() {
                // Respect the master PIC mask: the driver masks its own IRQ line
                // for the duration of its ISR. Ignoring it re-delivers mid-ISR and
                // corrupts the saved return frame. Keep the edge pending (do not
                // consume) so it fires once the driver unmasks.
                // INT 0x08..0x0F are master IRQ 0..7 (port 0x02, bit = vec-0x08).
                if (0x08..=0x0F).contains(&v) && self.io.pic_mask & (1 << (v - 0x08)) != 0 {
                    self.dbg_masked += 1;
                    return false;
                }
                self.io.opn.take_irq_edge();
                self.opn_in_service = true;
                self.cpu.interrupt(v);
                self.irqs += 1;
                return true;
            }
            self.dbg_no_vec += 1;
        }
        false
    }

    /// Run until the CPU consumes `budget` cycles or a stopping condition.
    /// Handles INT traps, the timer IRQ, and the wait-for-interrupt idle. When
    /// `stop_on_stub_ready`, returns [`PumpEnd::StubReady`] the moment a funcvect
    /// glue stub signals ready — via its `0x7E8 == 0x81` handshake write — so
    /// setup ends there whether the stub then HLT-idles (PMD_98) or spins on a
    /// poll loop (MLP_HOOT and other `*_HOOT` glue stubs).
    fn pump(&mut self, budget: u64, burst: u32, stop_on_stub_ready: bool) -> PumpEnd {
        let start = self.cycle;
        let mut ticks = 0u32;
        while self.cycle - start < budget {
            // Wall-clock guard: a spinning driver (never HLTs) executes the whole
            // emulated budget instruction by instruction — bound it so a sweep
            // can't stall on a non-working set. Checked coarsely to stay cheap.
            if let Some(dl) = self.deadline {
                ticks = ticks.wrapping_add(1);
                if ticks % 1024 == 0 && std::time::Instant::now() >= dl {
                    self.timed_out = true;
                    return PumpEnd::Budget;
                }
            }
            // Generic stub-ready: the glue stub writes STUB_READY to its handshake
            // port once its INT handler is installed. Catch it here so a stub that
            // then busy-polls (rather than HLT-idling) still ends setup cleanly.
            if stop_on_stub_ready && self.io.stub_state == super::io::STUB_READY {
                return PumpEnd::StubReady;
            }
            self.io.now = self.cycle;
            if self.deliver_timer_irq() {
                continue;
            }
            let (cycles, stop) = self.cpu.run(&mut *self.io, burst);
            self.cycle += cycles as u64;
            self.advance_timers(cycles as u64);
            if stop == Stop::Halted {
                let cs = self.cpu.reg16(Reg16::Cs);
                let ip = self.cpu.reg16(Reg16::Ip);
                if let Some(vec) = self.dos.trap_vector(cs, ip) {
                    // (Stub-ready is caught generically at the loop top, before
                    // the burst, so a stub idling on INT 18h ends setup there.)
                    if let Some(res) = self.dos.service_int(&mut *self.cpu, vec) {
                        return PumpEnd::Ended(res);
                    }
                    self.dos.iret_return(&mut *self.cpu);
                } else {
                    // A genuine wait-for-interrupt idle: nothing will change
                    // until a timer fires, so advance the clocks (and the
                    // timestamp base) rather than spinning.
                    self.cycle += IDLE_QUANTUM_CPU;
                    self.advance_timers(IDLE_QUANTUM_CPU);
                }
            }
        }
        PumpEnd::Budget
    }

    /// Far-call `entry_cs:entry_ip` with `ES:BX` preset, returning once the
    /// callee `RETF`s to the harness sentinel [`TRAMP_SEG`]:[`CALL_RET_OFF`].
    /// Runs on a private scratch stack; register state is otherwise left as the
    /// callee leaves it (this runs between DOS setup and the shell chain, so
    /// there is no caller context to preserve). This drives a DOS device
    /// driver's STRATEGY / INTERRUPT entry points through INIT — the driver
    /// installs its API/timer vectors via INT 21h AH=25h, which we service in
    /// the loop. Returns false if it did not return within `budget` cycles.
    fn call_far(&mut self, entry_cs: u16, entry_ip: u16, es: u16, bx: u16, budget: u64) -> bool {
        // Return sentinel the far RETF lands on (a HLT the loop recognizes).
        self.cpu.mem()[lin(TRAMP_SEG, CALL_RET_OFF)] = 0xF4;
        // Build the far-return frame (RETF pops IP then CS).
        let mut sp = DEV_STACK_SP;
        sp = sp.wrapping_sub(2);
        wr16(self.cpu.mem(), lin(DEV_STACK_SEG, sp), TRAMP_SEG); // return CS
        sp = sp.wrapping_sub(2);
        wr16(self.cpu.mem(), lin(DEV_STACK_SEG, sp), CALL_RET_OFF); // return IP
        self.cpu.set_ss_sp(DEV_STACK_SEG, sp);
        self.cpu.set_reg16(Reg16::Es, es);
        self.cpu.set_reg16(Reg16::Bx, bx);
        // Device drivers are entered with DS == CS (the driver segment): some
        // store the request pointer with a plain `mov [0x34],bx` (no CS
        // override) and read it back with `lds bx,[cs:0x34]`, so DS must equal
        // CS or the pointer round-trips through the wrong segment and INIT wild-
        // jumps. Setting DS is harmless for drivers that use CS overrides.
        self.cpu.set_reg16(Reg16::Ds, entry_cs);
        // INIT does its own CLI/STI; start with IF set so its INT 21h calls run.
        let fl = self.cpu.reg16(Reg16::Flags) | flag::IF;
        self.cpu.set_reg16(Reg16::Flags, fl);
        self.cpu.set_cs_ip(entry_cs, entry_ip);

        let start = self.cycle;
        while self.cycle - start < budget {
            self.io.now = self.cycle;
            // Deliver timer IRQs during INIT: some drivers calibrate their tempo
            // by installing a temporary INT 08h (PIT) ISR and spinning until it
            // has fired a couple of times (D98M.SYS), so INIT never returns
            // unless the timer ticks are pumped through.
            if self.deliver_timer_irq() {
                continue;
            }
            let (cycles, stop) = self.cpu.run(&mut *self.io, SETUP_BURST);
            self.cycle += cycles as u64;
            self.advance_timers(cycles as u64);
            if stop == Stop::Halted {
                let cs = self.cpu.reg16(Reg16::Cs);
                let ip = self.cpu.reg16(Reg16::Ip);
                if cs == TRAMP_SEG && ip == CALL_RET_OFF {
                    return true; // returned to the sentinel — done
                }
                if let Some(vec) = self.dos.trap_vector(cs, ip) {
                    if self.dos.service_int(&mut *self.cpu, vec).is_some() {
                        // INIT shouldn't Terminate/TSR; treat as done to be safe.
                        return true;
                    }
                    self.dos.iret_return(&mut *self.cpu);
                } else {
                    // Unexpected HLT idle mid-INIT: advance clocks (bounded by
                    // the budget) rather than spin.
                    self.cycle += IDLE_QUANTUM_CPU;
                    self.advance_timers(IDLE_QUANTUM_CPU);
                }
            }
        }
        false
    }
}

/// Load a program image and run it to its ending condition (or the budget).
fn run_command(
    eng: &mut Engine,
    name: &str,
    dos_image: (Vec<u8>, ProgKind),
    tail: &[u8],
    budget: u64,
) -> StepResult {
    let (image, kind) = dos_image;
    let load = match kind {
        ProgKind::Com => eng.dos.load_com(eng.cpu, name, &image, tail),
        ProgKind::Exe => eng.dos.load_exe(eng.cpu, name, &image, tail),
    };
    if let Err(e) = load {
        return StepResult::Error(e.to_string());
    }
    match eng.pump(budget, SETUP_BURST, true) {
        PumpEnd::Ended(ExecResult::Terminated(code)) => StepResult::Terminated(code),
        PumpEnd::Ended(ExecResult::Resident) => StepResult::Resident,
        PumpEnd::StubReady => StepResult::StubReady,
        PumpEnd::Budget => StepResult::RanToBudget,
    }
}

/// Bind each rom to the DOS handle equal to its `offset`, as hoot does.
///
/// hoot presents a set's files to the driver on fixed DOS handles: the game's
/// system EXE on handle 5 (its `conin` rom's offset), the songs on handles 0x10+
/// (each song's offset is also its title code). Self-contained players that read
/// and validate a file at INSTALL time — USD_98 (handle 5 = ADVBIOS.EXE),
/// MLALF_98 (handle 5 = FP_SYS.EXE), SDN_98 (handle 9 = SDN.COM), Brandish, … —
/// branch to a bare idle loop (installing no ISR, writing no StubReady handshake)
/// when that handle is unbound. Replicating the offset→handle map lets them
/// install. `offset == -1` (0xFFFF…) is an executable, not a handle; skip it, and
/// skip anything outside the small-handle range so we never collide with the
/// standard handles a program opens itself.
fn bind_rom_handles(dos: &mut MiniDos, romlist: &RomList) {
    for rom in &romlist.roms {
        if !matches!(rom.kind.as_str(), "file" | "conin") {
            continue;
        }
        let Some(off) = rom.offset else { continue };
        if off > 0x30 {
            continue; // -1 executable, or not a plausible DOS handle number
        }
        let base = rom.name.rsplit(['\\', '/', ':']).next().unwrap_or(&rom.name);
        // A `conin` rom presents the referenced FILENAME string on its handle
        // (hoot's console-input semantics for these driver families); a `file`
        // rom presents the file's content. The usd_98/mlalf_98 engines read
        // handle 5 as a filename to open + overlay-load — see set_handle_text.
        if rom.kind == "conin" {
            dos.set_handle_text(off as u16, base);
        } else {
            dos.set_handle(off as u16, base);
        }
    }
}

/// Build the DOS device-driver INIT request header (command 0) at
/// [`DEV_REQ_SEG`]:0 and its CR-terminated argument string at [`DEV_ARG_SEG`]:0.
///
/// DOS passes a device driver a far pointer (request header offset 0x12) to the
/// text that follows the driver's filename on its CONFIG.SYS `DEVICE=` line —
/// i.e. the arguments, led by the separating space and terminated by CR. The
/// arg parsers skip that leading space, then read their (decimal) parameter, so
/// a driver with no arguments gets `" \r"` and falls back to its default.
fn build_init_request(cpu: &mut dyn X86Cpu, args: &str) {
    let req = lin(DEV_REQ_SEG, 0);
    let mem = cpu.mem();
    for b in mem[req..req + 26].iter_mut() {
        *b = 0;
    }
    mem[req] = 26; // length of the request header
    mem[req + 2] = 0; // command 0 = INIT
    // Far pointer at +0x12 to the argument string.
    wr16(mem, req + 0x12, 0x0000); // arg offset
    wr16(mem, req + 0x14, DEV_ARG_SEG); // arg segment
    let mut s = String::from(" ");
    s.push_str(args.trim());
    s.push('\r');
    let arg = lin(DEV_ARG_SEG, 0);
    for (i, b) in s.bytes().enumerate() {
        mem[arg + i] = b;
    }
    mem[arg + s.len()] = 0; // NUL after the CR for parsers that scan for zero
}

/// Load and INIT every `<rom type="device">` DOS character-device driver, in
/// listed order, before the shell chain — the emulated equivalent of the
/// CONFIG.SYS `DEVICE=` lines these sets rely on. Each driver's INIT installs
/// its sound-API (and any timer-ISR) interrupt vectors, which the glue stub
/// then calls; without loading them, sets whose sound engine is a device driver
/// (MMD2.SYS → INT D2h, D98M.SYS, SNDDRV2.SYS, …) install nothing and stay
/// silent. Returns a note per driver for diagnostics.
fn load_device_drivers(eng: &mut Engine, romlist: &RomList, budget: u64) -> Vec<String> {
    let mut notes = Vec::new();
    for rom in romlist.roms.iter().filter(|r| r.kind == "device") {
        // The rom name is "FILE.SYS [args]" (the CONFIG.SYS DEVICE= remainder).
        let (fname, args) = match rom.name.split_once(char::is_whitespace) {
            Some((f, a)) => (f, a),
            None => (rom.name.as_str(), ""),
        };
        let base = fname.rsplit(['\\', '/', ':']).next().unwrap_or(fname);
        let Some(image) = eng.dos.file_bytes(base) else {
            notes.push(format!("[device {base}: not materialized]"));
            continue;
        };
        if image.len() < 12 {
            notes.push(format!("[device {base}: too small ({} B)]", image.len()));
            continue;
        }
        let seg = match eng.dos.load_device_image(eng.cpu, &image) {
            Ok(s) => s,
            Err(e) => {
                notes.push(format!("[device {base}: {e}]"));
                continue;
            }
        };
        // Read the device header from the LOADED image: +6 strategy entry
        // offset, +8 interrupt entry offset. (For an MZ `.EXE` device the header
        // lives at the start of the relocated load module, not at image[0].)
        let strat = rd16(eng.cpu.mem_ref(), lin(seg, 6));
        let intr = rd16(eng.cpu.mem_ref(), lin(seg, 8));
        // MUSE device drivers (NMUSE/MUSE2/SDD) choose their API interrupt vector
        // by reading OPN SSG register 0x0E (a board-config jumper) during INIT.
        // The paired muse_98.com stub then locates the resident driver via the
        // INT 0x14 segment word (0000:0052). Force the jumper bits (7-6 = 11b) so
        // all three drivers select INT 0x14; otherwise they hook INT 0x0B/0x15 and
        // the stub reads a bogus segment, wild-jumps, and never installs its play
        // vector (→ silent). The 8-char device name lives at the loaded header +10.
        let dev_name = {
            let m = eng.cpu.mem_ref();
            let s = lin(seg, 10);
            String::from_utf8_lossy(&m[s..s + 8]).to_ascii_uppercase()
        };
        if dev_name.contains("MUSE") || dev_name.contains("SDD") {
            eng.io.opn.set_ssg_readback(0x0E, 0xC0);
        }
        build_init_request(eng.cpu, args);
        // STRATEGY (records the request-header pointer), then INTERRUPT (runs
        // the INIT command handler: probes hardware, installs vectors, sets the
        // resident break address).
        eng.call_far(seg, strat, DEV_REQ_SEG, 0, budget);
        eng.call_far(seg, intr, DEV_REQ_SEG, 0, budget);
        // Read the returned break address (resident end) and status, then shrink
        // the driver's block to what it actually kept so later program loads sit
        // clear of it.
        let mem = eng.cpu.mem_ref();
        let brk_off = rd16(mem, lin(DEV_REQ_SEG, 0x0E));
        let brk_seg = rd16(mem, lin(DEV_REQ_SEG, 0x10));
        let status = rd16(mem, lin(DEV_REQ_SEG, 0x03));
        if brk_seg > seg && brk_seg < ARENA_END {
            let paras = (brk_seg - seg).saturating_add((brk_off + 15) >> 4);
            let _ = eng.dos.resize(eng.cpu.mem(), seg, paras.max(0x10));
        }
        notes.push(format!(
            "[device {base} seg={seg:04X} strat={strat:04X} intr={intr:04X} break={brk_seg:04X}:{brk_off:04X} status={status:04X}]"
        ));
    }
    notes
}

/// Read a set file case-insensitively (XML vs on-disk casing differ). Some sets
/// name a song under a subdirectory (`DEMO/USAUSA.BGM`); the driver still opens
/// it by basename, so if the file is not directly in the set dir, search one
/// level of subdirectories for a case-insensitive basename match.
fn read_set_file(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let base = name.rsplit(['\\', '/', ':']).next().unwrap_or(name);
    let direct = dir.join(base);
    if direct.is_file() {
        return std::fs::read(&direct).with_context(|| format!("reading {}", direct.display()));
    }
    // Honor an explicit relative subpath from the XML (e.g. `DEMO/USAUSA.BGM`).
    let rel = name.replace('\\', "/");
    let relp = dir.join(&rel);
    if relp.is_file() {
        return std::fs::read(&relp).with_context(|| format!("reading {}", relp.display()));
    }
    let lower = base.to_lowercase();
    if let Some(hit) = find_basename(dir, &lower, 2) {
        return std::fs::read(&hit).with_context(|| format!("reading {}", hit.display()));
    }
    bail!("file {name:?} not found in {}", dir.display())
}

/// Search `dir` (and up to `depth` levels of subdirectories) for a file whose
/// name case-insensitively equals `lower`. Directory-first breadth is fine for
/// the shallow set trees here.
fn find_basename(dir: &Path, lower: &str, depth: u32) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if entry.file_name().to_string_lossy().to_lowercase() == lower {
            return Some(path);
        }
    }
    if depth == 0 {
        return None;
    }
    for sub in subdirs {
        if let Some(hit) = find_basename(&sub, lower, depth - 1) {
            return Some(hit);
        }
    }
    None
}

/// Rip one title from a `pc98dos` game. First pass is diagnostic-forward: it
/// runs the shell chain, auto-detects the sound vector, attempts a capture, and
/// returns everything observed so the strategy can be tuned per driver family.
pub fn rip_title(
    game: &Game,
    set_dir: &Path,
    title_code: u64,
    opts: &Pc98RipOptions,
) -> Result<Pc98RipOutcome> {
    let romlist = game.romlist.as_ref().context("game has no romlist")?;

    // OPN vs OPNA by driver kind: "opna"/"86" are YM2608, else YM2203.
    let kind = game.driver.kind.as_deref().unwrap_or("opn");
    let is_opna = matches!(kind, "opna" | "86");
    let opn_hz = opts.opn_clock_hz.unwrap_or(if is_opna { PC98_OPNA_CLOCK_HZ } else { PC98_OPN_CLOCK_HZ });
    let cpu_hz = PC98_CPU_HZ as u64;

    let mut cpu = Np2Cpu::new();
    cpu.reset();
    cpu.set_adrsmask(0x000F_FFFF); // 8086/286 real mode, 1 MB
    cpu.set_v30(false); // i286 semantics (PMD_98.COM uses SHR imm)
    // The NP2 core's memory is a process global; wipe conventional RAM so a
    // previous rip in the same process can't leak stale code/vectors.
    for b in cpu.mem()[0..0xA_0000].iter_mut() {
        *b = 0;
    }

    let mut dos = MiniDos::new();
    dos.init_arena(cpu.mem());
    dos.install_trampolines(cpu.mem());
    set_sound_bios(&mut cpu, opts.dummy_sndrom);
    dos.install_dos_structures(cpu.mem());

    let mut notes = materialize(&mut dos, romlist, set_dir);
    let mut song_file = selected_song(romlist, title_code);

    // FM-variant: the Vermouth/TGLFMP2 sets play `.MGS` MIDI data through FMP3 in
    // `-m` mode (melody → MIDI, OPN only keeps time). The original FM soundtrack
    // ships alongside as `.MFM`. Bind the `.MFM` sibling instead and drop `-m` from
    // the fmp shell (below) so FMP3 installs as an FM driver and writes the chip.
    let fm_variant = opts.fm_variant;
    if fm_variant {
        if let Some(sf) = &song_file {
            let stem = sf.rsplit_once('.').map(|(s, _)| s).unwrap_or(sf);
            let mfm = format!("{stem}.MFM");
            match read_set_file(set_dir, &mfm) {
                Ok(data) => {
                    dos.add_file(&mfm, data);
                    song_file = Some(mfm);
                }
                Err(e) => eprintln!("[fm-variant] no .MFM for {sf}: {e}"),
            }
        }
    }

    // --- Engine -----------------------------------------------------------
    let mut io = Pc98Io::new(opn_hz, is_opna);
    let mut eng = Engine {
        cpu: &mut cpu,
        dos: &mut dos,
        io: &mut io,
        cpu_hz,
        opn_hz: opn_hz as u64,
        opn_residual: 0,
        cycle: 0,
        forced_vec: opts.sound_vector,
        funcvect: opts.funcvect,
        irqs: 0,
        opn_timer_used: false,
        pit_timer_used: false,
        dbg_if_clear: 0,
        dbg_no_vec: 0,
        dbg_masked: 0,
        opn_in_service: false,
        bios_timer: None,
        deadline: opts
            .deadline_secs
            .map(|s| std::time::Instant::now() + std::time::Duration::from_secs_f64(s)),
        timed_out: false,
    };

    // --- Run the shell-command chain --------------------------------------
    // The `#` song-placeholder token is dropped from each command ([`strip_hash`]);
    // every glue-stub family feeds the song to the resident driver via DOS handle 0
    // at trigger time (PMD.COM etc. are options-only installers that reject a bare
    // filename → exit 255), so `pmd #/k` must become `pmd /k`.
    // Some glue stubs read the selected song during their INSTALL (not just at
    // the play trigger) — MAKO_98 reads DOS handle 0x0B to size its buffers and
    // skips installing its INT handler if that read returns nothing. Provide the
    // song on that handle before the shell chain so the install completes.
    // (PMD_98/MLP_HOOT read handle 0/5 only at trigger time — set in capture.
    // Do NOT pre-bind 0/5 here: it makes USD_98 read further into its unbound
    // song format but does not complete its parse, and it breaks sets that read
    // handle 0 at install expecting it empty, e.g. Tokyo Twilight Busters.)
    bind_rom_handles(eng.dos, romlist);
    if let Some(sf) = &song_file {
        if std::env::var_os("HOOTRIP_NO_0B").is_none() {
            eng.dos.set_handle(0x0B, sf);
        }
    }
    preset_muse_irq_jumper(&mut eng, romlist);
    let setup_budget = (opts.setup_seconds * cpu_hz as f64) as u64;
    // Load CONFIG.SYS-style device drivers first: their INIT installs the sound
    // API the shell stub then drives (e.g. MMD2.SYS → INT D2h).
    for n in load_device_drivers(&mut eng, romlist, setup_budget) {
        notes.push_str(&n);
        notes.push('\n');
    }
    let mut shell = Vec::new();
    for rom in romlist.roms.iter().filter(|r| r.kind == "shell") {
        let mut cmd = strip_hash(&rom.name);
        if fm_variant {
            // Drop the MIDI-mode switch so FMP3 installs as an FM driver.
            if cmd.to_lowercase().starts_with("fmp") {
                cmd = cmd
                    .split_whitespace()
                    .filter(|tok| !tok.eq_ignore_ascii_case("-m") && !tok.eq_ignore_ascii_case("m"))
                    .collect::<Vec<_>>()
                    .join(" ");
            }
        }
        let (name, tail) = split_cmd(&cmd);
        let before = eng.cycle;
        let result = match eng.dos.resolve_program(name) {
            Some(img) => run_command(&mut eng, name, img, tail.as_bytes(), setup_budget),
            None => StepResult::Error(format!("program {name:?} not found in set")),
        };
        shell.push(ShellStep { cmd, result, cycles: eng.cycle - before });
    }

    // Snapshot the memory map now that the drivers are resident.
    let mcb_chain = eng.dos.mcb_chain(eng.cpu.mem_ref());

    // --- Capture ----------------------------------------------------------
    let capture_budget = (opts.seconds * cpu_hz as f64) as u64;
    // A glue stub is resident and awaiting the trigger whenever one of hoot's
    // externalCommand vectors (0x7E/0x7F) is installed — regardless of whether
    // the stub announced itself with the 0x7E8 handshake (PMD_98) or not
    // (MLP_HOOT just installs INT 7Fh and spins). Prefer the explicit option.
    let trigger_vec = opts
        .funcvect
        .or_else(|| [0x7Eu8, 0x7F].into_iter().find(|v| eng.dos.installed_vectors.contains_key(v)));
    if let Some(vec) = trigger_vec {
        // funcvect playback: hand the selected song to the DOS handle the glue
        // stub reads, set its virtual command/song ports, and invoke the funcvect
        // INT — the stub loads the data and starts the driver, whose timer ISR we
        // then pace to record the register stream.
        bind_trigger_song(&mut eng, romlist, &song_file, title_code);
        eng.io.capturing = true;
        let f = eng.cpu.reg16(Reg16::Flags) | flag::IF;
        eng.cpu.set_reg16(Reg16::Flags, f);
        eng.cpu.interrupt(vec);
    } else {
        // No glue stub: park on an idle HLT and pace whatever went resident.
        install_idle_stub(eng.cpu);
        eng.io.capturing = true;
    }
    let t0 = eng.cycle;
    // Snapshot the voice/operator register state the driver set up before the
    // capture window, to replay at t=0 so the render starts from the same chip
    // state (see Opn::seed_writes) — otherwise pre-t0 timbre/stereo-enable is
    // lost and the render can be silent despite a correct note stream.
    let seed = eng.io.opn.seed_writes();
    eng.pump(capture_budget, CAPTURE_BURST, false);

    // The pacing timer actually in use: the PIT's INT 08h if it drove IRQs,
    // else the OPN chip timer's vector.
    let sound_vector = if eng.pit_timer_used {
        Some(PC98_TIMER_VEC)
    } else {
        eng.opn_sound_vec()
    };
    let irqs = eng.irqs;
    let opn_timer_used = eng.opn_timer_used;
    let pit_timer_used = eng.pit_timer_used;
    let dbg_if_clear = eng.dbg_if_clear;
    let dbg_no_vec = eng.dbg_no_vec;
    let timed_out = eng.timed_out;

    if std::env::var_os("HOOTRIP_DBG_D2").is_some() {
        let ta = eng.io.opn.timer_a_debug();
        let tb = eng.io.opn.timer_b_debug();
        let (ofa, ofb) = eng.io.opn.overflow_counts();
        eprintln!(
            "[dbg] irqs={} masked={} no_vec={} if_clear={}  TimerA(en={} per={} flag={} of={})  TimerB(en={} per={} flag={} of={})",
            eng.irqs, eng.dbg_masked, dbg_no_vec, dbg_if_clear,
            ta.1, ta.3, ta.4, ofa, tb.1, tb.3, tb.4, ofb
        );
    }

    // --- Assemble diagnostics + log ---------------------------------------
    let mut hooked_vectors = BTreeMap::new();
    for v in 0u16..256 {
        let (seg, off) = eng.ivt(v as u8);
        if seg != TRAMP_SEG && seg != 0 {
            hooked_vectors.insert(v as u8, (seg, off));
        }
    }

    // Declare YM2608 whenever the driver actually drove OPN bank-1 (FM ch 4-6 /
    // rhythm / ADPCM), even for an "opn"-kind set — several drivers (NAX, NA, …)
    // use OPNA extended channels regardless of the XML chip tag, and a YM2203
    // device can't carry port-1 writes (the VGM/S98 writers would reject them).
    let use_opna = is_opna || eng.io.opn.port1_used;
    let device = if use_opna {
        Device { chip: Chip::Ym2608, clock_hz: opn_hz }
    } else {
        Device { chip: Chip::Ym2203, clock_hz: opn_hz }
    };
    let mut log = RegisterLog::new(cpu_hz, vec![device]);
    let has_capture = io.writes.iter().any(|w| w.t >= t0);
    if has_capture {
        // Prime the render chip with the pre-capture voice/operator state.
        for (bank, addr, data) in &seed {
            log.push(RegWrite { t: 0, dev: 0, port: *bank, addr: *addr, data: *data });
        }
    }
    for w in io.writes.iter().filter(|w| w.t >= t0) {
        log.push(RegWrite { t: w.t - t0, ..*w });
    }
    log.end_t = capture_budget;

    let mut console = notes;
    console.push_str(&console_text(&dos.con_out));

    Ok(Pc98RipOutcome {
        log,
        shell,
        hooked_vectors,
        installed_vectors: dos.installed_vectors.clone(),
        unimpl: dos.unimpl.clone(),
        console,
        unknown_ports: io.unknown.clone(),
        sound_vector,
        irqs,
        total_fm_writes: io.total_writes,
        captured_writes: io.writes.iter().filter(|w| w.t >= t0).count(),
        opn_timer_used,
        pit_timer_used,
        opn_end_state: (io.opn.irq(), io.opn.timer_a_debug(), io.opn.timer_b_debug()),
        dbg_if_clear,
        dbg_no_vec,
        mcb_chain,
        timed_out,
    })
}

/// Materialize a set's `file` roms into the virtual disk and apply `binary`
/// byte patches. Returns notes about any files that were missing on disk.
fn materialize(dos: &mut MiniDos, romlist: &RomList, set_dir: &Path) -> String {
    let mut last_file: Option<String> = None;
    let mut notes = String::new();
    for rom in &romlist.roms {
        match rom.kind.as_str() {
            // `conin` roms name a file too (the game's system EXE, fed on a DOS
            // handle == the rom's `offset`); materialize it like a `file` rom so
            // an install-time-reading stub can validate it. See bind_rom_handles.
            "file" | "conin" => {
                let base = rom.name.rsplit(['\\', '/', ':']).next().unwrap_or(&rom.name);
                match read_set_file(set_dir, &rom.name) {
                    Ok(data) => {
                        dos.add_file(base, data);
                        last_file = Some(base.to_uppercase());
                    }
                    Err(e) => notes.push_str(&format!("[missing file {}: {e}]\n", rom.name)),
                }
            }
            "binary" => {
                if let (Some(off), Some(val), Some(fname)) =
                    (rom.offset, parse_num(&rom.name), last_file.as_ref())
                {
                    dos.patch_file(fname, off as usize, val as u8);
                }
            }
            _ => {}
        }
    }
    notes
}

/// True if `name` looks like an executable/driver rather than song data — used
/// to keep the `conin` song fallback in [`selected_song`] from ever selecting a
/// family's engine (dofmdx/usd_98/mlalf_98 present their `.EXE` engine on a
/// `conin` handle) as if it were a song.
fn is_engine_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.ends_with(".EXE") || n.ends_with(".COM") || n.ends_with(".DRV") || n.ends_with(".SYS")
}

/// The song data file that the `#` token / title code selects for `title_code`.
///
/// Matched by `offset` against the title code (whole, then low byte). `file`
/// roms win first; if none matches, a `conin` rom at the low-byte offset is a
/// candidate too: several glue-stub families (cplay98, mbmusp, mdrv_98) list
/// their song banks as `conin` roms (offset != -1). The song is materialized on
/// disk (a paired `file offset="-1"` rom), so the resolved name is either bound
/// as content on handle 0 (mbmusp/mdrv_98 — the stub reads the whole file) or as
/// a filename the driver opens itself (cplay98) — see [`bind_trigger_song`].
/// Engine `conin` roms (dofmdx/usd_98/mlalf_98 put their `.EXE` on a conin
/// handle, usually offset 5) are excluded so a low byte that collides with an
/// engine offset can never select the engine as the "song".
fn selected_song(romlist: &RomList, title_code: u64) -> Option<String> {
    let low = (title_code & 0xFF) as i64;
    romlist
        .roms
        .iter()
        .find(|r| r.kind == "file" && r.offset == Some(title_code as i64))
        .or_else(|| romlist.roms.iter().find(|r| r.kind == "file" && r.offset == Some(low)))
        .or_else(|| {
            romlist
                .roms
                .iter()
                .find(|r| r.kind == "conin" && r.offset == Some(low) && !is_engine_name(&r.name))
        })
        .map(|r| r.name.rsplit(['\\', '/', ':']).next().unwrap_or(&r.name).to_string())
}

/// A PC-98 guest writes its console in **Shift_JIS**, and peppers it with ANSI
/// colour escapes. Decoding it as UTF-8 replaces every Japanese message with
/// question marks, which is how "there is no sound board" — the driver telling
/// you exactly why it gave up — reaches a diagnostic as unreadable noise.
fn console_text(bytes: &[u8]) -> String {
    let text = hoot_xml::decode_shift_jis(bytes);
    // Strip CSI sequences (ESC [ ... final byte): they colour the reader's own
    // terminal and carry nothing about what the driver did.
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Bind the selected song to the DOS handle(s) the driver reads and set the
/// funcvect selection ports (0x7E0 cmd / 0x7E2 song / 0x7E4 param), honoring
/// per-family delivery conventions. Shared by the rip and trace capture paths.
///
/// Two song-delivery conventions:
/// * **cplay98/FPLAY family** — the driver opens the song *by name*: its API
///   (INT 7Fh AH=9) treats the string on handle 0 as an ASCIIZ filename, opens
///   and reads the whole `.dat` bank itself, and plays the 0-based in-bank index
///   from port 0x7E4 (title byte 2). So handle 0 carries the *filename text* and
///   0x7E4 carries byte 2.
/// * **everyone else** — the stub reads the whole song *content* from handle 0
///   (and 5 / 0x0B for install-time readers) and hands the driver a buffer. Bank
///   drivers (MAKO) read byte 2 as a 1-based song number on 0x7E2, byte 1 as the
///   load param on 0x7E4; one-file-per-song drivers use the low byte on 0x7E2.
fn bind_trigger_song(eng: &mut Engine, romlist: &RomList, song_file: &Option<String>, title_code: u64) {
    let shell_starts = |prefixes: &[&str]| {
        romlist.roms.iter().any(|r| {
            r.kind == "shell" && {
                let n = r.name.to_ascii_lowercase();
                prefixes.iter().any(|p| n.starts_with(p))
            }
        })
    };
    // Drivers that open the song file *themselves* by an ASCIIZ name — FPLAY2
    // (cplay98, INT 7Fh AH=9), MUSDRV (mbmusp, INT 68h AH=1), and MDRV (the
    // acidplan MDRV.COM + `mdrv_98`/`mddrv_98` stub, INT D2h AL=2 "load song file
    // by name") all `int21 AX=3D00` a string on/derived from handle 0, so handle
    // 0 must carry the *filename text*, not the file content. (Each stub reads
    // handle 0 into a buffer and passes it as the "filename".) Match the stub
    // `mdrv_98`/`mddrv_98` specifically — NOT the unrelated `mdrv98`+`mlp_hoot`
    // family, which streams song *content* on handle 0 via INT F2h and must keep
    // the content path. cplay98 additionally plays a 0-based in-bank index off
    // port 0x7E4 (title byte 2); MUSDRV/MDRV are one file per song.
    let cplay_family = shell_starts(&["cplay", "fplay"]);
    // EMD (`emd_98`) joins them: its stub reads handle 0 into a buffer, writes a
    // NUL at the byte count it just got back, and passes that to INT D2h AH=1 —
    // terminating a *string*, which is only meaningful for a filename. Handed
    // content instead, AH=1 fails, the stub takes its `jnz` exit and returns
    // without ever calling the play entry, leaving a timer ticking over silence.
    let opens_by_name =
        cplay_family || shell_starts(&["musdrv", "mbmusp", "mdrv_9", "mddrv_9", "emd_98"]);
    if let Some(sf) = song_file {
        if opens_by_name {
            eng.dos.set_handle_text(0, sf);
        } else {
            eng.dos.set_handle(0, sf);
            eng.dos.set_handle(5, sf);
            eng.dos.set_handle(0x0B, sf); // MAKO_98 and kin read the song here
        }
    }
    eng.io.ext_cmd = 0;
    let byte2 = ((title_code >> 16) & 0xFF) as u16;
    // MDR external-voice family: the small MDR.EXE build keeps FM timbres in an
    // external `.VOI` file. Title byte 2 = the DOS handle that file is bound on;
    // the stub reads that handle *number* from port 0x7E4 and loads the voice
    // (already bound as content by bind_rom_handles) before playing the song on
    // handle 0. Self-identify by a `.VOI` rom sitting at offset == byte 2.
    let mdr_voice = byte2 != 0
        && romlist
            .roms
            .iter()
            .any(|r| r.offset == Some(byte2 as i64) && r.name.to_ascii_uppercase().ends_with(".VOI"));
    // Packed ARTDI variant: songs live inside one `.PAC` file bound on the DOS
    // handle named by title byte 1; the low byte is the in-pack index. ARTDI_98's
    // INT 7Fh handler takes its packed branch when the HIGH byte of port 0x7E2 is
    // nonzero — it seeks that handle (byte 1) and indexes the low byte itself via
    // the pack's LE32 size table (COM 0x1A3 / PackSeek 0x206), reading one song
    // into its buffer. So present the full 0x??SS word; handle byte1 already holds
    // the pack content (bind_rom_handles), nothing goes on handle 0. Self-identify
    // by a `.PAC` file rom at offset == byte 1.
    let byte1 = ((title_code >> 8) & 0xFF) as u16;
    let packed = byte1 != 0
        && romlist.roms.iter().any(|r| {
            r.kind == "file"
                && r.offset == Some(byte1 as i64)
                && r.name.to_ascii_uppercase().ends_with(".PAC")
        });
    if cplay_family {
        eng.io.ext_song = 0;
        eng.io.ext_param = byte2; // byte 2 → 0x7E4 = in-bank song index
    } else if mdr_voice {
        eng.io.ext_song = 0;
        eng.io.ext_param = byte2; // byte 2 → 0x7E4 = voice-file DOS handle
    } else if packed {
        eng.io.ext_song = (title_code & 0xFFFF) as u16; // 0x7E2: high=pack handle, low=index
        eng.io.ext_param = 0;
    } else {
        // Title-code → song selection. The low byte picks the file/conin rom
        // (bound above). When byte 2 is set the driver is bank-style — one
        // multi-song file, byte 2 = 1-based song number, byte 1 = the 0x7E4
        // load-path param (MAKO). Otherwise it is one file per song and the low
        // byte is the song word (PMD).
        if byte2 != 0 {
            eng.io.ext_song = byte2;
            eng.io.ext_param = ((title_code >> 8) & 0xFF) as u16;
        } else {
            eng.io.ext_song = (title_code & 0xFF) as u16;
            eng.io.ext_param = 0;
        }
    }
}

/// Preset the OPN SSG reg-0x0E board-config jumper (bits 7-6 = 11b) that the
/// mbmusp driver (MUSDRV) reads during its board detect to choose its IRQ
/// vector. With the default 0x00 readback MUSDRV hooks INT 0x0B (master IRQ3),
/// but its ISR EOIs/unmasks the *slave* PIC, so it enables the wrong PIC and the
/// master IRQ3 stays masked → the OPN-timer IRQ is never delivered (song starts
/// then freezes). 0xC0 makes it select INT 0x14 (slave IRQ12), consistent with
/// its slave EOI. Applied before the shell chain, since MUSDRV reads the jumper
/// at install. (The MUSE *device* drivers get the same preset in
/// load_device_drivers, keyed on the device header name.)
fn preset_muse_irq_jumper(eng: &mut Engine, romlist: &RomList) {
    let mbmusp = romlist.roms.iter().any(|r| {
        r.kind == "shell" && {
            let n = r.name.to_ascii_lowercase();
            n.starts_with("musdrv") || n.starts_with("mbmusp")
        }
    });
    if mbmusp {
        eng.io.opn.set_ssg_readback(0x0E, 0xC0);
    }
}

/// A single-step execution trace of one shell command, for driver bring-up.
pub struct TraceReport {
    /// The command line traced.
    pub cmd: String,
    /// Instructions single-stepped.
    pub steps: u64,
    /// Hottest linear PCs (address, hit count), most-hit first — reveals the
    /// spin loop a stalled driver is stuck in.
    pub hot: Vec<(u32, u64)>,
    /// The first software interrupts hit, in order: (vector, AH, linear PC).
    pub int_seq: Vec<(u8, u8, u32)>,
    /// Total count of each interrupt vector serviced.
    pub int_counts: BTreeMap<u8, u64>,
    /// OPN data writes during the trace.
    pub fm_writes: u64,
    /// Whether the trace stopped early because it detected a tight spin.
    pub stalled: bool,
    /// Console output produced during the trace (lossy UTF-8).
    pub console: String,
    /// Unmodelled port traffic during the trace.
    pub unknown_ports: BTreeMap<u16, (u64, u64)>,
}

/// Single-step-trace one shell command (default the first — the driver) to see
/// where control flows and where it stalls. Prior shell commands are *not* run;
/// this isolates one program's startup for diagnosis.
pub fn trace_title(
    game: &Game,
    set_dir: &Path,
    title_code: u64,
    cmd_index: usize,
    max_steps: u64,
    opts: &Pc98RipOptions,
    cmd_override: Option<&str>,
) -> Result<TraceReport> {
    let romlist = game.romlist.as_ref().context("game has no romlist")?;
    let kind = game.driver.kind.as_deref().unwrap_or("opn");
    let is_opna = matches!(kind, "opna" | "86");
    let opn_hz = opts.opn_clock_hz.unwrap_or(if is_opna { PC98_OPNA_CLOCK_HZ } else { PC98_OPN_CLOCK_HZ });

    let mut cpu = Np2Cpu::new();
    cpu.reset();
    cpu.set_adrsmask(0x000F_FFFF);
    cpu.set_v30(false);
    for b in cpu.mem()[0..0xA_0000].iter_mut() {
        *b = 0;
    }
    let mut dos = MiniDos::new();
    dos.init_arena(cpu.mem());
    dos.install_trampolines(cpu.mem());
    set_sound_bios(&mut cpu, opts.dummy_sndrom);
    dos.install_dos_structures(cpu.mem());
    let _ = materialize(&mut dos, romlist, set_dir);
    // Bind rom→handle as the capture path does, so `--trace` reflects the real
    // install path (an install-time-reading stub takes a different branch when a
    // handle is unbound — without this the trace is actively misleading).
    bind_rom_handles(&mut dos, romlist);
    let _song = selected_song(romlist, title_code);

    let cmd = match cmd_override {
        Some(c) => c.to_string(),
        None => romlist
            .roms
            .iter()
            .filter(|r| r.kind == "shell")
            .nth(cmd_index)
            .map(|r| strip_hash(&r.name))
            .with_context(|| format!("no shell command at index {cmd_index}"))?,
    };
    let (name, tail) = split_cmd(&cmd);
    let (image, prog) = dos
        .resolve_program(name)
        .with_context(|| format!("program {name:?} not found"))?;
    match prog {
        ProgKind::Com => dos.load_com(&mut cpu, name, &image, tail.as_bytes())?,
        ProgKind::Exe => dos.load_exe(&mut cpu, name, &image, tail.as_bytes())?,
    };

    let mut io = Pc98Io::new(opn_hz, is_opna);
    let cpu_hz = PC98_CPU_HZ as u64;
    let mut opn_residual = 0u64;
    let mut hist: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let mut int_seq = Vec::new();
    let mut int_counts: BTreeMap<u8, u64> = BTreeMap::new();
    let mut steps = 0u64;
    let mut cycles = 0u64;
    let mut stalled = false;
    // The PC-98 timer-BIOS one-shot, tracked exactly as the rip path tracks it
    // so a `--trace` of a driver that calibrates against it sees the same run.
    let mut bios_timer: Option<(u16, u16, u64)> = None;

    // Spin detection: sample the hottest PC every window. A window dominated by
    // one PC is only a *stall* if no timer IRQ fired in it — a busy-wait that is
    // being serviced by the timer (e.g. PMD's calibration delay) is progress.
    let window = 200_000u64;
    let mut win_hist: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let mut win_irqs = 0u64;

    while steps < max_steps {
        match dos.bios_timer.take() {
            Some(BiosTimerReq::Arm { seg, off, ticks }) => {
                bios_timer = Some((seg, off, cycles + (ticks.max(1) as u64) * PC98_BIOS_TICK_CPU));
            }
            Some(BiosTimerReq::Cancel) => bios_timer = None,
            None => {}
        }
        // Deliver a pending timer IRQ between instructions, as hardware would.
        if cpu.reg16(Reg16::Flags) & flag::IF != 0 {
            if let Some((seg, off, due)) = bios_timer {
                if cycles >= due {
                    bios_timer = None;
                    enter_far(&mut cpu, seg, off);
                    win_irqs += 1;
                    steps += 1;
                    continue;
                }
            }
            if io.pit.irq_pending && io.irq0_unmasked() && trace_hooked(&cpu, PC98_TIMER_VEC) {
                io.pit.ack();
                cpu.interrupt(PC98_TIMER_VEC);
                win_irqs += 1;
                *int_counts.entry(PC98_TIMER_VEC).or_insert(0) += 1;
                steps += 1;
                continue;
            }
            if io.vsync_pending && io.pic_mask & 0x04 == 0 && trace_hooked(&cpu, PC98_VSYNC_VEC) {
                io.vsync_pending = false;
                cpu.interrupt(PC98_VSYNC_VEC);
                win_irqs += 1;
                *int_counts.entry(PC98_VSYNC_VEC).or_insert(0) += 1;
                steps += 1;
                continue;
            }
            if io.opn.irq() {
                if let Some(v) = trace_opn_vec(&cpu, opts.funcvect) {
                    io.opn.ack_irq();
                    cpu.interrupt(v);
                    win_irqs += 1;
                    steps += 1;
                    continue;
                }
            }
        }

        let cs = cpu.reg16(Reg16::Cs);
        let ip = cpu.reg16(Reg16::Ip);
        let pc = ((cs as u32) << 4).wrapping_add(ip as u32);
        *hist.entry(pc).or_insert(0) += 1;
        *win_hist.entry(pc).or_insert(0) += 1;

        let (c, stop) = cpu.run(&mut io, 1);
        steps += 1;
        let elapsed = c.max(1) as u64;
        opn_residual += elapsed * opn_hz as u64;
        let ot = opn_residual / cpu_hz;
        opn_residual %= cpu_hz;
        if ot > 0 {
            io.opn.tick(ot);
        }
        io.pit.tick(elapsed, cpu_hz);
        io.tick_vsync(elapsed, cpu_hz);
        cycles += elapsed;

        if stop == Stop::Halted {
            let cs2 = cpu.reg16(Reg16::Cs);
            let ip2 = cpu.reg16(Reg16::Ip);
            if let Some(vec) = dos.trap_vector(cs2, ip2) {
                let ah = (cpu.reg16(Reg16::Ax) >> 8) as u8;
                if int_seq.len() < 200 {
                    int_seq.push((vec, ah, pc));
                }
                *int_counts.entry(vec).or_insert(0) += 1;
                if dos.service_int(&mut cpu, vec).is_some() {
                    // Program terminated / went resident during the trace.
                    break;
                }
                dos.iret_return(&mut cpu);
            } else {
                // Wait-for-interrupt idle: advance the clocks so a timer can
                // fire and wake it (rather than treating it as a stall).
                io.pit.tick(IDLE_QUANTUM_CPU, cpu_hz);
                io.tick_vsync(IDLE_QUANTUM_CPU, cpu_hz);
                cycles += IDLE_QUANTUM_CPU;
            }
        }

        if steps % window == 0 {
            if let Some((_, &top)) = win_hist.iter().max_by_key(|(_, c)| **c) {
                if top as f64 / window as f64 > 0.90 && win_irqs == 0 {
                    stalled = true;
                    break;
                }
            }
            win_hist.clear();
            win_irqs = 0;
        }
    }

    let mut hot: Vec<(u32, u64)> = hist.into_iter().collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1));
    hot.truncate(20);

    Ok(TraceReport {
        cmd,
        steps,
        hot,
        int_seq,
        int_counts,
        fm_writes: io.total_writes,
        stalled,
        console: console_text(&dos.con_out),
        unknown_ports: io.unknown.clone(),
    })
}

/// Result of tracing the funcvect capture phase.
pub struct CaptureTrace {
    /// Instructions single-stepped after the funcvect trigger.
    pub steps: u64,
    /// Hottest linear PCs (address, hits).
    pub hot: Vec<(u32, u64)>,
    /// Hottest PCs visited while a timer IRQ was pending but interrupts were
    /// disabled — the code responsible for a timer deadlock.
    pub frozen: Vec<(u32, u64)>,
    /// Control-flow transitions into the high buffer region (from_pc, to_pc) —
    /// wild jumps into data.
    pub wild_jumps: Vec<(u32, u32)>,
    /// Driver API calls observed: (INT vector, AH, AL) each time control entered
    /// a hooked handler (PMD's INT 60h dispatcher / the stub's INT 7Eh).
    pub api_calls: Vec<(u8, u8, u8)>,
    /// DOS read calls during the phase: (handle, bytes).
    pub reads: Vec<(u16, usize)>,
    /// OPN data writes captured.
    pub fm_writes: u64,
    /// Timer IRQs delivered.
    pub irqs: u64,
    /// Shell-chain outcomes leading up to the trigger.
    pub shell: Vec<(String, String)>,
}

/// Run the full setup + funcvect trigger, then single-step-trace the capture to
/// see what the glue stub and driver actually do (which API functions run,
/// whether the song data was read, where the CPU spends its time).
pub fn trace_capture(
    game: &Game,
    set_dir: &Path,
    title_code: u64,
    max_steps: u64,
    opts: &Pc98RipOptions,
) -> Result<CaptureTrace> {
    let romlist = game.romlist.as_ref().context("game has no romlist")?;
    let kind = game.driver.kind.as_deref().unwrap_or("opn");
    let is_opna = matches!(kind, "opna" | "86");
    let opn_hz = opts.opn_clock_hz.unwrap_or(if is_opna { PC98_OPNA_CLOCK_HZ } else { PC98_OPN_CLOCK_HZ });
    let cpu_hz = PC98_CPU_HZ as u64;

    let mut cpu = Np2Cpu::new();
    cpu.reset();
    cpu.set_adrsmask(0x000F_FFFF);
    cpu.set_v30(false);
    for b in cpu.mem()[0..0xA_0000].iter_mut() {
        *b = 0;
    }
    let mut dos = MiniDos::new();
    dos.init_arena(cpu.mem());
    dos.install_trampolines(cpu.mem());
    set_sound_bios(&mut cpu, opts.dummy_sndrom);
    dos.install_dos_structures(cpu.mem());
    let _ = materialize(&mut dos, romlist, set_dir);
    let mut song_file = selected_song(romlist, title_code);
    let fm_variant = opts.fm_variant;
    if fm_variant {
        if let Some(sf) = &song_file {
            let stem = sf.rsplit_once('.').map(|(s, _)| s).unwrap_or(sf);
            let mfm = format!("{stem}.MFM");
            if let Ok(data) = read_set_file(set_dir, &mfm) {
                dos.add_file(&mfm, data);
                song_file = Some(mfm);
            }
        }
    }

    let mut io = Pc98Io::new(opn_hz, is_opna);
    let mut eng = Engine {
        cpu: &mut cpu,
        dos: &mut dos,
        io: &mut io,
        cpu_hz,
        opn_hz: opn_hz as u64,
        opn_residual: 0,
        cycle: 0,
        forced_vec: opts.sound_vector,
        funcvect: opts.funcvect,
        irqs: 0,
        opn_timer_used: false,
        pit_timer_used: false,
        dbg_if_clear: 0,
        dbg_no_vec: 0,
        dbg_masked: 0,
        opn_in_service: false,
        bios_timer: None,
        deadline: None,
        timed_out: false,
    };

    // Setup: run the shell chain (as rip_title does).
    // Some glue stubs read the selected song during their INSTALL (not just at
    // the play trigger) — MAKO_98 reads DOS handle 0x0B to size its buffers and
    // skips installing its INT handler if that read returns nothing. Provide the
    // song on that handle before the shell chain so the install completes.
    // (PMD_98/MLP_HOOT read handle 0/5 only at trigger time — set in capture.
    // Do NOT pre-bind 0/5 here: it makes USD_98 read further into its unbound
    // song format but does not complete its parse, and it breaks sets that read
    // handle 0 at install expecting it empty, e.g. Tokyo Twilight Busters.)
    bind_rom_handles(eng.dos, romlist);
    if let Some(sf) = &song_file {
        if std::env::var_os("HOOTRIP_NO_0B").is_none() {
            eng.dos.set_handle(0x0B, sf);
        }
    }
    preset_muse_irq_jumper(&mut eng, romlist);
    let setup_budget = (opts.setup_seconds * cpu_hz as f64) as u64;
    // Load CONFIG.SYS-style device drivers first (as rip_title does).
    let mut shell = Vec::new();
    for n in load_device_drivers(&mut eng, romlist, setup_budget) {
        shell.push(("device-load".to_string(), n));
    }
    for rom in romlist.roms.iter().filter(|r| r.kind == "shell") {
        let mut cmd = strip_hash(&rom.name);
        if fm_variant && cmd.to_lowercase().starts_with("fmp") {
            cmd = cmd
                .split_whitespace()
                .filter(|tok| !tok.eq_ignore_ascii_case("-m") && !tok.eq_ignore_ascii_case("m"))
                .collect::<Vec<_>>()
                .join(" ");
        }
        let (name, tail) = split_cmd(&cmd);
        let result = match eng.dos.resolve_program(name) {
            Some(img) => run_command(&mut eng, name, img, tail.as_bytes(), setup_budget),
            None => StepResult::Error(format!("program {name:?} not found")),
        };
        shell.push((cmd, format!("{result:?}")));
    }

    // Trigger the funcvect play (as rip_title does).
    let trigger_vec = opts
        .funcvect
        .or_else(|| [0x7Eu8, 0x7F].into_iter().find(|v| eng.dos.installed_vectors.contains_key(v)));
    if let Some(vec) = trigger_vec {
        bind_trigger_song(&mut eng, romlist, &song_file, title_code);
        eng.io.capturing = true;
        let f = eng.cpu.reg16(Reg16::Flags) | flag::IF;
        eng.cpu.set_reg16(Reg16::Flags, f);
        eng.cpu.interrupt(vec);
    } else {
        install_idle_stub(eng.cpu);
        eng.io.capturing = true;
    }

    // Addresses of the driver / stub API handlers, to log entries into them.
    let api60 = {
        let (s, o) = eng.ivt(0x60);
        ((s as u32) << 4).wrapping_add(o as u32)
    };
    // FMP3 (TGLFMP2 family) is driven via INT D2h, not INT 60h.
    let api_d2 = {
        let (s, o) = eng.ivt(0xD2);
        ((s as u32) << 4).wrapping_add(o as u32)
    };
    if std::env::var_os("HOOTRIP_DBG_D2").is_some() {
        let (s, o) = eng.ivt(0xD2);
        eprintln!("[dbg] INT D2h handler = {s:#06x}:{o:#06x}  (linear {api_d2:#07x})");
        let base = (s as u32) << 4; // FMP3 data seg base (DS = PSP seg)
        let m = eng.cpu.mem();
        let rd = |off: u32| -> u16 {
            let a = (base + off) as usize;
            m[a] as u16 | ((m[a + 1] as u16) << 8)
        };
        eprintln!(
            "[dbg] flags: [1e65]FM={:#x} [1e69]MIDIen={:#x} [1e6b]songFM={:#x} [1e6d]songMIDI={:#x} [2a4f]tA={:#x} [20ab]timerMode={:#x} [1a09]={:#x}",
            rd(0x1e65), rd(0x1e69), rd(0x1e6b), rd(0x1e6d), rd(0x2a4f), rd(0x20ab), rd(0x1a09)
        );
    }
    let read_base = eng.dos.read_log.len();

    // Single-step the capture, recording hot PCs and API-handler entries.
    let mut hist: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    // PCs visited while the OPN IRQ is pending but interrupts are disabled — i.e.
    // the code that deadlocks the timer.
    let mut freeze_hist: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let mut api_calls = Vec::new();
    // Transitions from driver/stub code into the high buffer region — a wild
    // jump into data: (from_pc, to_pc).
    let mut wild_jumps: Vec<(u32, u32)> = Vec::new();
    let mut prev_pc = 0u32;
    let mut opn_residual = 0u64;
    let mut steps = 0u64;
    while steps < max_steps {
        eng.io.now = eng.cycle;
        if eng.deliver_timer_irq() {
            continue;
        }
        let cs = eng.cpu.reg16(Reg16::Cs);
        let ip = eng.cpu.reg16(Reg16::Ip);
        let pc = ((cs as u32) << 4).wrapping_add(ip as u32);
        // Valid code lives in the PMD image, the stub image, or low system
        // memory (trampolines / idle stub). A jump anywhere else is wild.
        let in_code = |p: u32| {
            p < 0x10000 || (0x10100..0x15300).contains(&p) || (0x20100..0x20900).contains(&p)
        };
        if !in_code(pc) && in_code(prev_pc) && wild_jumps.len() < 40 {
            wild_jumps.push((prev_pc, pc));
        }
        prev_pc = pc;
        *hist.entry(pc).or_insert(0) += 1;
        if eng.io.opn.irq() && eng.cpu.reg16(Reg16::Flags) & flag::IF == 0 {
            *freeze_hist.entry(pc).or_insert(0) += 1;
        }
        if pc == api60 && api_calls.len() < 400 {
            let ax = eng.cpu.reg16(Reg16::Ax);
            api_calls.push((0x60u8, (ax >> 8) as u8, ax as u8));
        }
        if pc == api_d2 && api_calls.len() < 400 {
            let ax = eng.cpu.reg16(Reg16::Ax);
            api_calls.push((0xD2u8, (ax >> 8) as u8, ax as u8));
        }

        let (c, stop) = eng.cpu.run(&mut *eng.io, 1);
        steps += 1;
        let elapsed = c.max(1) as u64;
        opn_residual += elapsed * opn_hz as u64;
        let ot = opn_residual / cpu_hz;
        opn_residual %= cpu_hz;
        if ot > 0 {
            eng.io.opn.tick(ot);
        }
        eng.io.pit.tick(elapsed, cpu_hz);
        eng.io.tick_vsync(elapsed, cpu_hz);
        eng.cycle += elapsed;

        if stop == Stop::Halted {
            let cs2 = eng.cpu.reg16(Reg16::Cs);
            let ip2 = eng.cpu.reg16(Reg16::Ip);
            if let Some(vec) = eng.dos.trap_vector(cs2, ip2) {
                if eng.dos.service_int(&mut *eng.cpu, vec).is_some() {
                    break;
                }
                eng.dos.iret_return(&mut *eng.cpu);
            } else {
                eng.cycle += IDLE_QUANTUM_CPU;
                eng.io.pit.tick(IDLE_QUANTUM_CPU, cpu_hz);
                eng.io.tick_vsync(IDLE_QUANTUM_CPU, cpu_hz);
            }
        }
    }

    let mut hot: Vec<(u32, u64)> = hist.into_iter().collect();
    hot.sort_by(|a, b| b.1.cmp(&a.1));
    hot.truncate(20);
    let mut frozen: Vec<(u32, u64)> = freeze_hist.into_iter().collect();
    frozen.sort_by(|a, b| b.1.cmp(&a.1));
    frozen.truncate(20);
    let reads = eng.dos.read_log[read_base..].to_vec();
    let fm_writes = eng.io.total_writes;
    let irqs = eng.irqs;

    Ok(CaptureTrace { steps, hot, frozen, wild_jumps, api_calls, reads, fm_writes, irqs, shell })
}

/// Install or remove the PC-98 sound BIOS's presence byte.
///
/// Always written, never merely set: the NP2 core's memory is a process global
/// and the per-rip wipe covers only conventional RAM (`0..0xA_0000`), so a byte
/// left in the BIOS ROM window by one set is still there for the next set in
/// the same process. `pc98-sweep` runs hundreds of sets that way, and a driver
/// that finds this byte behaves differently — so a set with `dummysndrom` would
/// silently turn on the sound BIOS for every set swept after it.
fn set_sound_bios(cpu: &mut dyn X86Cpu, present: bool) {
    cpu.mem()[PC98_SNDROM_INT_OFF] = if present { PC98_SNDROM_INT } else { 0 };
}

/// Enter `seg:off` the way hardware enters an interrupt handler — flags, CS and
/// IP pushed on the guest's own stack, IF cleared — so the routine's `IRET`
/// returns to whatever it interrupted. `X86Cpu::interrupt` only dispatches
/// through the IVT; a BIOS callback has no vector of its own.
fn enter_far(cpu: &mut dyn X86Cpu, seg: u16, off: u16) {
    let ss = cpu.reg16(Reg16::Ss);
    let flags = cpu.reg16(Reg16::Flags);
    let frame = [flags, cpu.reg16(Reg16::Cs), cpu.reg16(Reg16::Ip)];
    let mut sp = cpu.reg16(Reg16::Sp);
    for v in frame {
        sp = sp.wrapping_sub(2);
        wr16(cpu.mem(), lin(ss, sp), v);
    }
    cpu.set_ss_sp(ss, sp);
    cpu.set_reg16(Reg16::Flags, flags & !flag::IF);
    cpu.set_cs_ip(seg, off);
}

/// Whether IVT[`vec`] points somewhere other than the harness trampoline.
fn trace_hooked(cpu: &dyn X86Cpu, vec: u8) -> bool {
    let m = cpu.mem_ref();
    let b = vec as usize * 4;
    let seg = m[b + 2] as u16 | ((m[b + 3] as u16) << 8);
    seg != TRAMP_SEG && seg != 0
}

/// The lowest hooked hardware-IRQ vector for the OPN chip timer (excluding the
/// PIT's INT 08h and the driver funcvect).
fn trace_opn_vec(cpu: &dyn X86Cpu, funcvect: Option<u8>) -> Option<u8> {
    for v in 0x08u8..=0x1F {
        if v == PC98_TIMER_VEC || Some(v) == funcvect {
            continue;
        }
        if trace_hooked(cpu, v) {
            return Some(v);
        }
    }
    None
}

/// Drop hoot's `#` song-placeholder token from a shell command.
///
/// hoot uses `#` as a bare placeholder token for "the selected song", but every
/// glue-stub family feeds the song to the resident driver via DOS handle 0, so
/// the placeholder always resolves to nothing (`pmd #/k` → `pmd /k`). Crucially,
/// `#` is ALSO a legitimate option character for some drivers — FMP's playback
/// buffer is `/#<KB>` (`FMP S /S /#12`), the MDZ family uses `-#8`/`/#8` — so we
/// must only strip a `#` that BEGINS a whitespace-delimited token (the placeholder
/// position) and preserve any `#` that appears mid-option (after `/`, `-`, …).
fn strip_hash(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut at_token_start = true;
    for c in cmd.chars() {
        if c == '#' && at_token_start {
            continue; // hoot song placeholder — resolves to empty
        }
        out.push(c);
        at_token_start = c.is_whitespace();
    }
    out
}

/// Split a command line into (program, argument tail).
fn split_cmd(cmd: &str) -> (&str, String) {
    let cmd = cmd.trim();
    match cmd.split_once(char::is_whitespace) {
        Some((n, t)) => (n, t.trim_start().to_string()),
        None => (cmd, String::new()),
    }
}

/// Park the CPU on a one-byte `HLT` idle loop with a fresh stack and interrupts
/// enabled, so delivering a timer IRQ vectors into the resident ISR and the
/// following `IRET` returns to the same `HLT`.
fn install_idle_stub(cpu: &mut dyn X86Cpu) {
    cpu.mem()[lin(IDLE_SEG, 0)] = 0xF4; // HLT
    cpu.set_cs_ip(IDLE_SEG, 0);
    cpu.set_ss_sp(IDLE_SEG, 0xF000); // ~61 KB of stack headroom below the arena
    let f = cpu.reg16(Reg16::Flags) | flag::IF;
    cpu.set_reg16(Reg16::Flags, f);
}

#[cfg(test)]
mod console_tests {
    use super::console_text;

    /// The guest writes Shift_JIS. `from_utf8_lossy` turned odq_98's
    /// 「サウンドボードがありません！」 — "there is no sound board", the whole
    /// diagnosis for 192 silent titles — into a row of replacement characters.
    #[test]
    fn shift_jis_console_survives_and_ansi_does_not() {
        let sjis = b"\x1b[33m\x83T\x83E\x83\x93\x83h\x83{\x81[\x83h\x82\xaa\x82\xa0\x82\xe8\x82\xdc\x82\xb9\x82\xf1\x81I\x1b[37m";
        assert_eq!(console_text(sjis), "サウンドボードがありません！");
    }

    /// A bare ESC, or a CSI nobody terminated, must not eat the rest of the log.
    #[test]
    fn a_stray_escape_does_not_swallow_the_message() {
        assert_eq!(console_text(b"ok\x1bdone"), "ok\u{1b}done");
        assert_eq!(console_text(b"ok\x1b[999"), "ok");
    }
}
