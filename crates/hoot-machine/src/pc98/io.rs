//! PC-98 port I/O for the `pc98dos` harness.
//!
//! The sound board (PC-9801-26/86, and the built-in OPN) exposes the YM2203/
//! YM2608 as two register banks: `0x188`/`0x18A` = address/data for bank 0
//! (FM 1-3, SSG, timers), `0x18C`/`0x18E` = bank 1 (FM 4-6, ADPCM, rhythm on
//! OPNA). This mirrors the PC-88 bus front-end, so `hoot-chips::Opn` is reused
//! unchanged.
//!
//! Beyond the sound chip we model the pieces PC-98 sound drivers actually pace
//! themselves with: the **8253 PIT** (channel 0 on ports 0x71/0x77) whose
//! output drives IRQ0 → INT 08h, and the master **8259 PIC** interrupt mask
//! (port 0x02) that gates it. PMD and kin calibrate their tempo by counting
//! PIT ticks, so a working PIT is what turns a hung driver into a playing one.
//! Every other port is tallied for diagnostics.

use std::collections::BTreeMap;

use hoot_chips::Opn;
use hoot_cpu::IoBus;
use hoot_log::RegWrite;

/// PC-98 OPN(A) sound-board register ports.
pub const OPN_ADDR0: u16 = 0x188;
pub const OPN_DATA0: u16 = 0x18A;
pub const OPN_ADDR1: u16 = 0x18C;
pub const OPN_DATA1: u16 = 0x18E;

/// PC-98 8253 PIT ports: counter 0 data (0x71) and the control word (0x77).
/// Counters 1/2 (0x73/0x75) are accepted and ignored — drivers rarely use them
/// and none drive a sound IRQ.
pub const PIT_CT0: u16 = 0x71;
pub const PIT_CTRL: u16 = 0x77;
/// Master 8259 PIC: OCW2 / EOI (0x00) and OCW1 / interrupt mask (0x02).
pub const PIC_CMD: u16 = 0x00;
pub const PIC_MASK: u16 = 0x02;
/// Slave 8259 PIC: OCW2 / EOI (0x08) and OCW1 / mask (0x0A). PC-98 routes the
/// FM-board IRQ through the slave; drivers like FMP3 EOI here (and on the master).
pub const SLAVE_PIC_CMD: u16 = 0x08;
pub const SLAVE_PIC_MASK: u16 = 0x0A;
/// PC-98 MPU-401 MIDI UART: data (0xE0D0) and command/status (0xE0D2).
pub const MPU401_DATA: u16 = 0xE0D0;
pub const MPU401_CMD: u16 = 0xE0D2;

/// PC-98 I/O-delay port: any write wastes ~600 ns of bus time. Drivers pepper
/// their tight timing loops with `out 0x5F,al`; we accept and drop it.
pub const IO_DELAY: u16 = 0x5F;

/// PC-9801-86 sound board "PCM ID / control" register. Reading it returns the
/// board ID (non-0xFF ⇒ present); OPNA drivers (PMDB2) gate all FM output on
/// this. The low bits are the PCM/interrupt controls the driver writes back.
pub const SOUND86_ID: u16 = 0xA460;
/// 86-board ID value: any non-0xFF byte marks the board present. Real boards
/// return 0x40/0x50-class IDs; the driver masks the low 2 bits before use.
pub const SOUND86_ID_VALUE: u8 = 0x40;

/// PC-9801-86 PCM control/status register (0xA468). Bits 0-2 select the sample
/// rate and bit 3 resets the FIFO; those are write-only controls we keep only
/// so the driver's read-modify-write updates see their own bits back. **Bit 4
/// reads back as "the PCM FIFO wants more data"** — PMD86's IRQ handler spins
/// on it (`in al,dx / test al,0x10 / jnz`), refilling until the request drops.
pub const PCM86_CTRL: u16 = 0xA468;
/// Bit 4 of [`PCM86_CTRL`]: the FIFO is below its refill threshold. The 86
/// board's PCM is a separate DAC that no S98/VGM device can carry, so we model
/// a FIFO that never starves and always report it clear. The driver then skips
/// its PCM feed and gets on with sequencing the FM chip — which is the part we
/// can actually log. Reporting it set (what an unmodelled 0xFF read does) hangs
/// every PMD86 set in that refill loop.
pub const PCM86_FIFO_REQ: u8 = 0x10;

/// hoot's "externalCommand" virtual ports, read by a `funcvect` glue stub
/// (e.g. PMD_98.COM) inside its INT 7Eh handler: 0x7E0 = command byte,
/// 0x7E2 = song word (low byte = song, high byte 0), 0x7E8 = handshake state
/// (stub writes 0x80 while loading, 0x81 when installed and idle). These are
/// hoot conventions, not PC-98 hardware.
pub const EXT_CMD: u16 = 0x07E0;
pub const EXT_SONG: u16 = 0x07E2;
/// Extra selection parameter word (0x7E4). Bank-style drivers (MAKO) read it to
/// choose a load path (0 = load by song index); PMD-style stubs ignore it.
pub const EXT_PARAM: u16 = 0x07E4;
pub const EXT_STATE: u16 = 0x07E8;
/// Value the stub writes to [`EXT_STATE`] once its INT 7Eh handler is installed.
pub const STUB_READY: u8 = 0x81;

/// PC-98 PIT input clock for the common (2.5 MHz-derived) machine class. The
/// exact base (this vs 2.4576 MHz) is a per-model BIOS flag; this matches the
/// flag state the harness presents. Tempo calibration against a reference rip
/// is a TODO — see the harness.
pub const PIT_CLOCK_HZ: u32 = 1_996_800;

/// CRT vertical-sync ack port (PC-98). A frame-paced sequencer's VSYNC ISR
/// writes here to clear the retrace latch before `iret`.
pub const VSYNC_ACK: u16 = 0x0064;

/// PC-98 CRT vertical-sync (VSYNC) rate, delivered as IRQ2 → INT 0Ah. Frame-
/// paced engines (A-Train's ARTDI, VSYNCMAN/SNDDRV2) calibrate their timing
/// against the retrace and advance one sequencer tick per frame. ~60 Hz for
/// 200-line mode; the exact value only sets playback tempo, fine to approximate.
pub const PC98_VSYNC_HZ: u32 = 60;

/// One 8253 counter (we only need channel 0: the IRQ0 timer source).
pub struct Pit {
    pub clock_hz: u32,
    /// 16-bit reload value; 0 means 65536.
    reload: u16,
    /// Live down-counter in PIT clocks (fixed-point via `residual`).
    counter: u32,
    /// Access latch flip-flop for lo/hi byte writes.
    write_hi: bool,
    /// Access latch flip-flop for lo/hi byte reads.
    read_hi: bool,
    /// Latched counter value from a latch command, consumed by reads.
    latched: Option<u16>,
    /// Fractional CPU-cycle accumulator for CPU→PIT clock conversion.
    residual: u64,
    /// IRQ0 line: set when the counter wraps, cleared by [`Pit::ack`].
    pub irq_pending: bool,
    running: bool,
}

impl Pit {
    fn new(clock_hz: u32) -> Self {
        Pit {
            clock_hz,
            reload: 0,
            counter: 0,
            write_hi: false,
            read_hi: false,
            latched: None,
            residual: 0,
            irq_pending: false,
            running: false,
        }
    }

    fn reload_ticks(&self) -> u32 {
        if self.reload == 0 {
            0x1_0000
        } else {
            self.reload as u32
        }
    }

    /// Control-word write (port 0x77). We service channel-0 latch and the
    /// common lo/hi access mode; the operating mode only affects waveform
    /// symmetry, which does not matter for a rate/IRQ source.
    fn control(&mut self, v: u8) {
        let channel = v >> 6;
        if channel != 0 {
            return; // channels 1/2 unused for the sound IRQ
        }
        let access = (v >> 4) & 0x3;
        if access == 0 {
            // Counter latch command: freeze the current count for reading.
            self.latched = Some(self.counter as u16);
        } else {
            self.write_hi = false;
            self.read_hi = false;
        }
    }

    /// Counter-0 data write (port 0x71): lo then hi byte, then (re)load.
    fn write_counter(&mut self, v: u8) {
        if !self.write_hi {
            self.reload = (self.reload & 0xFF00) | v as u16;
            self.write_hi = true;
        } else {
            self.reload = (self.reload & 0x00FF) | ((v as u16) << 8);
            self.write_hi = false;
            self.counter = self.reload_ticks();
            self.residual = 0;
            self.running = true;
        }
    }

    /// Counter-0 data read (port 0x71): lo then hi of the latched or live count.
    fn read_counter(&mut self) -> u8 {
        let val = self.latched.unwrap_or(self.counter as u16);
        let byte = if !self.read_hi { val as u8 } else { (val >> 8) as u8 };
        if self.read_hi {
            self.latched = None; // both bytes consumed
        }
        self.read_hi = !self.read_hi;
        byte
    }

    /// Advance the counter by the PIT clocks equivalent to `cpu_cycles`. Sets
    /// `irq_pending` on each wrap (period ≫ the harness burst, so at most one
    /// wrap accrues per step and none are dropped).
    pub fn tick(&mut self, cpu_cycles: u64, cpu_hz: u64) {
        if !self.running {
            return;
        }
        self.residual += cpu_cycles * self.clock_hz as u64;
        let mut elapsed = (self.residual / cpu_hz) as u32;
        self.residual %= cpu_hz;
        while elapsed > 0 {
            if elapsed >= self.counter {
                elapsed -= self.counter;
                self.counter = self.reload_ticks();
                self.irq_pending = true;
            } else {
                self.counter -= elapsed;
                elapsed = 0;
            }
        }
    }

    pub fn ack(&mut self) {
        self.irq_pending = false;
    }
}

pub struct Pc98Io {
    pub opn: Opn,
    /// PC-98 system timer (8253 channel 0) → IRQ0.
    pub pit: Pit,
    /// Master 8259 interrupt mask (port 0x02): bit N set = IRQ N masked.
    pub pic_mask: u8,
    /// Slave 8259 interrupt mask (port 0x0A). Accepted for drivers that unmask
    /// the FM-board IRQ there; not otherwise consulted (we deliver directly).
    pub slave_pic_mask: u8,
    /// Set when the guest writes an EOI to a PIC command port; the harness reads
    /// and clears it to lift the OPN interrupt's in-service latch.
    pub eoi_seen: bool,
    /// 8259 ICW init state, master & slave: 0 = idle (a write to the mask port is
    /// OCW1), 1.. = the driver is mid-(re)init and the next mask-port writes are
    /// ICW2/ICW3/ICW4, NOT the mask. Sound drivers routinely re-init the PIC and
    /// never write a following OCW1, relying on ICW1 clearing the mask to 0 (all
    /// IRQs unmasked). Latching an ICW byte as the mask would wrongly mask the
    /// sound IRQ (e.g. ICW4=0x1d masks IRQ3 = INT 0x0B). `icw1` remembers the ICW1
    /// byte so bit 0 tells us whether an ICW4 word follows.
    master_icw: u8,
    master_icw1: u8,
    slave_icw: u8,
    slave_icw1: u8,
    /// Last-logged CH3-mode bits (0xC0) of OPN reg 0x27, to collapse the inaudible
    /// timer-control churn while still logging genuine CH3 special-mode changes.
    last_ch3_mode: Option<u8>,
    /// funcvect virtual-port state: the command byte (0x7E0), the song word
    /// (0x7E2) the harness sets before invoking INT 7Eh, and the stub handshake
    /// (0x7E8). `stub_state == STUB_READY` means the glue stub is installed.
    pub ext_cmd: u8,
    pub ext_song: u16,
    /// Extra selection param word read at [`EXT_PARAM`] (0x7E4).
    pub ext_param: u16,
    pub stub_state: u8,
    /// Timestamped register writes (only while `capturing`).
    pub writes: Vec<RegWrite>,
    /// Cycle timestamp for writes; the harness updates it per run burst.
    pub now: u64,
    /// Whether to record OPN writes (off during DOS setup, on during capture).
    pub capturing: bool,
    /// Total OPN data writes seen, regardless of `capturing` — a diagnostic that
    /// reveals whether the driver produced chip activity during DOS setup (e.g.
    /// a foreground player) versus only after going resident.
    pub total_writes: u64,
    /// Free-running strobe state for port 0x60 (bit 5 toggles each read). Some
    /// drivers (FMX) time their 86-board probe on that edge and spin forever if
    /// it never changes.
    port60: u8,
    /// VSYNC (IRQ2 → INT 0Ah) source. `vsync_residual` accumulates CPU cycles;
    /// `vsync_pending` latches a vertical retrace, cleared on IRQ delivery or a
    /// `0x64` ack write. Only frame-paced engines (which hook INT 0Ah and unmask
    /// IRQ2) ever consume it; see the harness's `deliver_timer_irq`.
    vsync_residual: u64,
    pub vsync_pending: bool,
    /// Unmodelled ports: port -> (reads, writes), for diagnostics.
    pub unknown: BTreeMap<u16, (u64, u64)>,
    /// Whether a PC-9801-86 board is present. OPNA drivers (PMDB2) probe the
    /// 86-board ID at [`SOUND86_ID`] and refuse to emit any FM output when it
    /// reads 0xFF; OPN-only sets model a machine without the board, so the ID
    /// must read 0xFF there to keep them on their 26-board/built-in path.
    pub has_86_board: bool,
    /// Last byte written to [`PCM86_CTRL`], handed back on read (minus the
    /// status bit) so the driver's read-modify-write rate/FIFO updates stick.
    pcm86_ctrl: u8,
    /// When set, log every OPN/detection port access to stderr (env-gated).
    pub io_debug: bool,
}

impl Pc98Io {
    pub fn new(opn_clock_hz: u32, has_86_board: bool) -> Self {
        Pc98Io {
            opn: Opn::new(opn_clock_hz),
            pit: Pit::new(PIT_CLOCK_HZ),
            pic_mask: 0xFF, // all IRQs masked until a driver unmasks
            slave_pic_mask: 0xFF,
            eoi_seen: false,
            master_icw: 0,
            master_icw1: 0,
            slave_icw: 0,
            slave_icw1: 0,
            last_ch3_mode: None,
            ext_cmd: 0,
            ext_song: 0,
            ext_param: 0,
            stub_state: 0,
            writes: Vec::new(),
            now: 0,
            capturing: false,
            total_writes: 0,
            unknown: BTreeMap::new(),
            has_86_board,
            port60: 0,
            pcm86_ctrl: 0,
            vsync_residual: 0,
            vsync_pending: false,
            io_debug: std::env::var_os("HOOTRIP_IO_DEBUG").is_some(),
        }
    }

    /// Whether IRQ0 (the PIT timer) is currently unmasked at the master PIC.
    pub fn irq0_unmasked(&self) -> bool {
        self.pic_mask & 0x01 == 0
    }

    /// Advance the VSYNC source by `cpu_cycles`, latching `vsync_pending` at the
    /// PC-98 vertical-retrace rate. Always accumulates (cheap); the latch is only
    /// acted on when a driver has hooked INT 0Ah and unmasked IRQ2.
    pub fn tick_vsync(&mut self, cpu_cycles: u64, cpu_hz: u64) {
        self.vsync_residual += cpu_cycles * PC98_VSYNC_HZ as u64;
        if self.vsync_residual >= cpu_hz {
            self.vsync_residual %= cpu_hz;
            self.vsync_pending = true;
        }
    }

    /// Consume the "an EOI was written" flag. The harness calls this each pump
    /// iteration to lift the OPN interrupt's in-service latch once the driver's
    /// ISR acknowledges the PIC.
    pub fn take_eoi(&mut self) -> bool {
        let e = self.eoi_seen;
        self.eoi_seen = false;
        e
    }

    fn record(&mut self) {
        if let Some((port, addr, data)) = self.opn.last_write.take() {
            self.total_writes += 1;
            if !self.capturing {
                return;
            }
            // Drop chip-timer bookkeeping that produces no sound. A VGM/S98 player
            // paces from the log's own wait commands, never the emulated OPN timers,
            // so Timer A/B period (0x24-0x26) is inaudible, and the timer-control
            // register (0x27) only matters for its CH3-mode bits (0xC0) — the
            // load/enable/reset bits are the driver's ISR bookkeeping. A driver like
            // FMP3 rewrites 0x27 on every timer interrupt (tens of thousands/sec),
            // which would otherwise bloat the log ~40x with inaudible writes.
            if port == 0 {
                match addr {
                    0x24 | 0x25 | 0x26 => return,
                    0x27 => {
                        // Log only when the CH3 mode bits change (audible); collapse
                        // the timer-control churn that keeps the same CH3 mode.
                        if self.last_ch3_mode == Some(data & 0xC0) {
                            return;
                        }
                        self.last_ch3_mode = Some(data & 0xC0);
                    }
                    _ => {}
                }
            }
            self.writes.push(RegWrite { t: self.now, dev: 0, port, addr, data });
        }
    }
}

impl IoBus for Pc98Io {
    fn out8(&mut self, port: u16, val: u8) {
        match port {
            OPN_ADDR0 => self.opn.write_addr(val),
            OPN_DATA0 => {
                self.opn.write_data(val);
                self.record();
            }
            OPN_ADDR1 => self.opn.write_addr1(val),
            OPN_DATA1 => {
                self.opn.write_data1(val);
                self.record();
            }
            PIT_CT0 => self.pit.write_counter(val),
            PIT_CTRL => self.pit.control(val),
            0x73 | 0x75 => {} // PIT counters 1/2: accepted, unused
            // PIC command port (0x00 master / 0x08 slave). Three cases:
            //  - ICW1 (bit 4 set): the driver is (re)initialising the 8259. Real
            //    hardware clears the IMR to 0 here; the following mask-port writes
            //    are ICW2/ICW3/ICW4, not the mask (tracked by `*_icw`). Drivers
            //    enable their sound IRQ this way and never write a later OCW1.
            //  - EOI (non-specific 0x20 / specific 0x60..=0x67): ISR finished →
            //    lift the OPN in-service latch so the next timer IRQ can deliver.
            //  - anything else (OCW2 rotate / OCW3 read IRR-ISR, e.g. 0x0B): ignore.
            PIC_CMD => {
                if val & 0x10 != 0 {
                    self.pic_mask = 0;
                    self.master_icw1 = val;
                    self.master_icw = 1;
                } else if val == 0x20 || (0x60..=0x67).contains(&val) {
                    self.eoi_seen = true;
                }
            }
            SLAVE_PIC_CMD => {
                if val & 0x10 != 0 {
                    self.slave_pic_mask = 0;
                    self.slave_icw1 = val;
                    self.slave_icw = 1;
                } else if val == 0x20 || (0x60..=0x67).contains(&val) {
                    self.eoi_seen = true;
                }
            }
            // Mask port (0x02 master / 0x0A slave). While mid-ICW-init these carry
            // ICW2/3/4 (consume them); otherwise the byte is the OCW1 mask. An
            // ICW1 with bit 0 set promises an ICW4, so the sequence is 3 or 4 words.
            PIC_MASK => {
                if self.master_icw != 0 {
                    self.master_icw += 1;
                    if self.master_icw >= 3 + (self.master_icw1 & 1) {
                        self.master_icw = 0;
                    }
                } else {
                    self.pic_mask = val;
                }
            }
            SLAVE_PIC_MASK => {
                if self.slave_icw != 0 {
                    self.slave_icw += 1;
                    if self.slave_icw >= 3 + (self.slave_icw1 & 1) {
                        self.slave_icw = 0;
                    }
                } else {
                    self.slave_pic_mask = val;
                }
            }
            EXT_CMD => self.ext_cmd = val,
            EXT_SONG => self.ext_song = (self.ext_song & 0xFF00) | val as u16,
            0x07E3 => self.ext_song = (self.ext_song & 0x00FF) | ((val as u16) << 8),
            EXT_PARAM => self.ext_param = (self.ext_param & 0xFF00) | val as u16,
            0x07E5 => self.ext_param = (self.ext_param & 0x00FF) | ((val as u16) << 8),
            EXT_STATE => self.stub_state = val,
            IO_DELAY => {} // I/O-delay port: timing padding, no state
            // CRT vertical-sync ack: a frame-paced engine's VSYNC ISR writes here
            // to clear the retrace latch before returning.
            VSYNC_ACK => self.vsync_pending = false,
            // 86-board PCM control (0xA460): bit 0 toggles OPNA 6-channel
            // ("extended") mode — the driver sets it right after board
            // detection to unlock FM ch 4-6. The bank-1 status/data ports read
            // 0xFF until this is on.
            SOUND86_ID => self.opn.set_extend(val & 0x01 != 0),
            // Other 86-board PCM/volume config the OPNA driver pokes (0xA46x
            // PCM ctrl/level, 0xA66x FM/PCM mixer, 0x6E PCM volume latch). We
            // log the music via the OPN registers, so these are accepted/dropped.
            PCM86_CTRL => self.pcm86_ctrl = val,
            0xA461..=0xA46F | 0xA660..=0xA66F | 0x6E => {}
            // MPU-401 MIDI UART (0xE0D0 data / 0xE0D2 command). Some FMP3 builds
            // reset the MIDI device during init even in FM mode; the melody goes
            // out as MIDI (which we do not capture), so accept and drop these.
            MPU401_DATA | MPU401_CMD => {}
            _ => {
                if self.io_debug {
                    eprintln!("[io] out {port:#06x} <- {val:#04x}");
                }
                self.unknown.entry(port).or_default().1 += 1;
            }
        }
    }

    fn in8(&mut self, port: u16) -> u8 {
        let v = match port {
            OPN_ADDR0 => self.opn.read_status(),
            // Bank-0 data readback (board86 opna_i18a): SSG regs read back, the
            // detection probe of register 0xFF returns 1, everything else 0.
            OPN_DATA0 => match self.opn.addr0() {
                a if a < 0x10 => self.opn.read_data(),
                0xFF => 1,
                _ => 0,
            },
            // Bank-1 status/data (board86 opna_i18c/opna_i18e): read 0xFF until
            // OPNA extended mode is enabled, then expose the ADPCM/ext status.
            OPN_ADDR1 => {
                if self.opn.extend() {
                    self.opn.read_status1()
                } else {
                    0xFF
                }
            }
            OPN_DATA1 => {
                if self.opn.extend() {
                    self.opn.read_data1()
                } else {
                    0xFF
                }
            }
            PIT_CT0 => self.pit.read_counter(),
            PIC_MASK => self.pic_mask,
            SLAVE_PIC_MASK => self.slave_pic_mask,
            // PIC IRR/ISR read (after an OCW3): report nothing in service. Drivers
            // (FMP3) read this to decide which PIC to EOI; returning 0 sends them
            // down the master-EOI path, which we detect to lift the in-service latch.
            PIC_CMD | SLAVE_PIC_CMD => 0,
            // MPU-401 status (0xE0D2): report the UART always ready to send and
            // with no data waiting (DSR/DRR both clear) so the driver's MIDI-init
            // handshake completes instead of spinning. Data port reads as empty.
            MPU401_CMD => 0x00,
            MPU401_DATA => 0x00,
            EXT_CMD => self.ext_cmd,
            EXT_SONG => self.ext_song as u8,
            0x07E3 => (self.ext_song >> 8) as u8,
            EXT_PARAM => self.ext_param as u8,
            0x07E5 => (self.ext_param >> 8) as u8,
            EXT_STATE => self.stub_state,
            // GDC status (0x60 text / 0xA0 graphics): expose a toggling bit 5
            // (VSYNC) so drivers that time on that edge advance instead of
            // spinning forever — FMX probes the 86-board on 0x60, and FMP (Guu)
            // calibrates its playback timing against the 0xA0 vertical-sync edge.
            0x60 | 0xA0 => {
                self.port60 ^= 0x20;
                self.port60
            }
            // 86-board PCM status (0xA468): the driver's own control bits back,
            // with the FIFO request always clear (see [`PCM86_FIFO_REQ`]).
            PCM86_CTRL => self.pcm86_ctrl & !PCM86_FIFO_REQ,
            // The rest of the 86-board PCM block (FIFO data, byte count, volume).
            // Read as 0 rather than falling through to 0xFF: a driver polling any
            // of these for a flag should see "nothing pending", not "all bits set".
            0xA461..=0xA46F => 0,
            // 86-board ID: non-0xFF ⇒ present (only when the set models one).
            SOUND86_ID => {
                if self.has_86_board {
                    SOUND86_ID_VALUE
                } else {
                    0xFF
                }
            }
            _ => {
                self.unknown.entry(port).or_default().0 += 1;
                0xFF
            }
        };
        if self.io_debug
            && matches!(port, OPN_ADDR0 | OPN_DATA0 | OPN_ADDR1 | OPN_DATA1 | SOUND86_ID)
        {
            eprintln!("[io] in  {port:#06x} (addr0={:#04x}) -> {v:#04x}", self.opn.addr0());
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PMD86's IRQ handler polls the 86-board FIFO request bit and refills until
    /// it drops (`in al,0xa468 / test al,0x10 / jnz`). An unmodelled read hands
    /// back 0xFF, so the bit is never clear and the driver never leaves the
    /// refill loop — that hung all 30 PC-9801-86 sets. The status bit must read
    /// clear no matter what the driver last wrote there.
    #[test]
    fn pcm86_fifo_never_asks_for_more_data() {
        let mut io = Pc98Io::new(3_993_600, true);
        assert_eq!(io.in8(PCM86_CTRL) & PCM86_FIFO_REQ, 0);
        // Even after the driver's own read-modify-write sets the bit.
        io.out8(PCM86_CTRL, 0xFF);
        assert_eq!(io.in8(PCM86_CTRL) & PCM86_FIFO_REQ, 0);
    }

    /// The rate/FIFO-reset bits are read-modify-written (`in / and / or / out`),
    /// so they have to survive the round trip or the driver's second update
    /// works from someone else's value.
    #[test]
    fn pcm86_control_bits_read_back() {
        let mut io = Pc98Io::new(3_993_600, true);
        io.out8(PCM86_CTRL, 0x05);
        assert_eq!(io.in8(PCM86_CTRL), 0x05);
        assert!(!io.unknown.contains_key(&PCM86_CTRL), "0xA468 must not count as unmodelled");
    }
}
