//! YM2203 (OPN) front-end: register file, address latch, SSG readback,
//! prescaler, and cycle-accurate Timer A/B with IRQ line.
//!
//! Timer timebase (per the YM2203/YM2608 application manuals): timers count
//! FM samples, one per `12 × prescale` chip clocks (prescale 6 after reset →
//! 72 clocks). Timer A period = (1024 − NA) samples, Timer B = 16 × (256 − NB)
//! samples (NB counts in 16-sample units → the familiar 1152 = 72×16 constant).

/// Bus-visible YM2203/YM2608. Time is advanced explicitly via [`Opn::tick`]
/// in chip clock cycles (on PC-88 the OPN shares the CPU crystal, so CPU
/// cycles work 1:1 when the clocks match).
///
/// Bank 1 (the OPNA extended registers at A1=1) is always accepted; whether
/// anything wrote to it is tracked in [`Opn::port1_used`] so callers can
/// decide to declare the log device as YM2608 instead of YM2203.
pub struct Opn {
    pub clock_hz: u32,
    regs: [[u8; 0x100]; 2],
    /// Which registers have ever been written, per bank — used to seed the
    /// capture log with the voice/operator state the driver programmed before
    /// the capture window began (see [`Opn::seed_writes`]).
    written: [[bool; 0x100]; 2],
    addr: [u8; 2],
    /// FM prescale (2, 3 or 6); reset value 6.
    prescale: u32,
    /// Chip-clock remainder not yet converted into timer steps.
    residual: u64,
    timer_a: Timer,
    timer_b: Timer,
    /// Set by the data-write methods for the caller to log: (port, reg, value).
    pub last_write: Option<(u8, u8, u8)>,
    /// True once any bank-1 (OPNA extended) register was written.
    pub port1_used: bool,
    /// Chip is a YM2608 (OPNA), not a YM2203 (OPN). fmgen's `OPNA::GetReg`
    /// returns 1 for register 0xFF as the board's identity byte; drivers such
    /// as MUCOM88 probe it (write 0xFF to the address port, read the data port,
    /// expect 1) to detect the PC-8801 Sound Board II. A plain OPN returns 0.
    opna: bool,
    /// OPNA 6-channel ("extended") mode. On the PC-9801-86 the bank-1 status
    /// and data ports read back 0xFF until the driver enables extended mode
    /// (via the board's PCM control bit); only then do they expose the ADPCM/
    /// extended-register state. Board detection depends on this 0xFF default.
    extend: bool,
    /// Last observed IRQ-line level (timer A or B flag), for edge detection.
    irq_line: bool,
    /// A rising edge of the IRQ line occurred since [`Opn::take_irq_edge`] was
    /// last called. A PC-98 PIC latches the edge, so the harness delivers one
    /// interrupt per edge regardless of how coarsely it polls — crucial when the
    /// CPU runs in bursts and a driver's ISR clears the flag mid-burst.
    irq_edge: bool,
}

struct Timer {
    running: bool,
    /// Status flag set on overflow (when enabled).
    flag: bool,
    /// Overflow sets the status flag only when enabled (reg 0x27 bits 2/3).
    enabled: bool,
    /// Countdown in timer steps (FM samples).
    counter: u32,
    period: u32,
    /// Debug: total overflows (whether or not the flag was enabled).
    overflows: u64,
}

impl Timer {
    fn new() -> Self {
        Timer { running: false, flag: false, enabled: false, counter: 0, period: 1, overflows: 0 }
    }

    fn step(&mut self, steps: u32) {
        if !self.running {
            return;
        }
        let mut remaining = steps;
        while remaining > 0 {
            if self.counter > remaining {
                self.counter -= remaining;
                return;
            }
            remaining -= self.counter;
            self.counter = self.period;
            self.overflows += 1;
            if self.enabled {
                self.flag = true;
            }
        }
    }
}

impl Opn {
    pub fn new(clock_hz: u32) -> Self {
        Opn {
            clock_hz,
            regs: [[0; 0x100]; 2],
            written: [[false; 0x100]; 2],
            addr: [0; 2],
            prescale: 6,
            residual: 0,
            timer_a: Timer::new(),
            timer_b: Timer::new(),
            last_write: None,
            port1_used: false,
            opna: false,
            extend: false,
            irq_line: false,
            irq_edge: false,
        }
    }

    /// Recompute the IRQ line after any flag change, latching a rising edge.
    fn note_irq_edge(&mut self) {
        let line = self.timer_a.flag || self.timer_b.flag;
        if line && !self.irq_line {
            self.irq_edge = true;
        }
        self.irq_line = line;
    }

    /// Consume the latched rising edge (returns true once per overflow that
    /// raised the line). Used by the harness to deliver exactly one interrupt.
    pub fn take_irq_edge(&mut self) -> bool {
        let e = self.irq_edge;
        self.irq_edge = false;
        e
    }

    /// Whether a rising edge is latched (without consuming it).
    pub fn peek_irq_edge(&self) -> bool {
        self.irq_edge
    }

    /// Debug: (Timer A overflows, Timer B overflows) since reset.
    pub fn overflow_counts(&self) -> (u64, u64) {
        (self.timer_a.overflows, self.timer_b.overflows)
    }

    /// Chip clocks per timer step at the current prescale.
    fn clocks_per_step(&self) -> u64 {
        12 * self.prescale as u64
    }

    fn timer_a_period(&self) -> u32 {
        let na = ((self.regs[0][0x24] as u32) << 2) | (self.regs[0][0x25] as u32 & 0x03);
        1024 - na
    }

    fn timer_b_period(&self) -> u32 {
        16 * (256 - self.regs[0][0x26] as u32)
    }

    /// Advance emulated time by `chip_cycles` chip clocks.
    pub fn tick(&mut self, chip_cycles: u64) {
        self.residual += chip_cycles;
        let cps = self.clocks_per_step();
        let steps = self.residual / cps;
        self.residual %= cps;
        if steps > 0 {
            let steps = steps.min(u32::MAX as u64) as u32;
            self.timer_a.step(steps);
            self.timer_b.step(steps);
            self.note_irq_edge();
        }
    }

    /// Write to the bank-0 address port (A1=0, A0=0).
    pub fn write_addr(&mut self, v: u8) {
        self.addr[0] = v;
        // Prescaler select registers take effect on address write alone.
        match v {
            0x2D => self.prescale = 6,
            0x2E => {
                if self.prescale == 6 {
                    self.prescale = 3;
                }
            }
            0x2F => self.prescale = 2,
            _ => {}
        }
    }

    /// Write to the bank-1 (OPNA extended) address port (A1=1, A0=0).
    pub fn write_addr1(&mut self, v: u8) {
        self.addr[1] = v;
    }

    /// Write to the bank-1 data port (A1=1, A0=1).
    pub fn write_data1(&mut self, v: u8) {
        let addr = self.addr[1];
        self.regs[1][addr as usize] = v;
        self.written[1][addr as usize] = true;
        self.port1_used = true;
        self.last_write = Some((1, addr, v));
    }

    /// Bank-1/extended status read (A1=1, A0=0): the YM2608 ADPCM flags OR'd with
    /// the main timer flags, matching `opna_readExtendedStatus`. BRDY (bit3, 0x08)
    /// and EOS (bit2, 0x04) are held ready — we log ADPCM register writes and have
    /// no sample engine, so any transfer/RAM-check loop that polls "buffer ready"
    /// (e.g. PMDB2's `PMD ADPCM RAM Check`, which spins on bit3) completes at once.
    /// The low two bits mirror the real Timer A/B flags via [`read_status`].
    pub fn read_status1(&self) -> u8 {
        0x0C | self.read_status()
    }

    /// Bank-1 data read: extended registers are write-only, returns 0.
    pub fn read_data1(&self) -> u8 {
        0
    }

    /// The persistent voice/operator register state to replay at the start of a
    /// capture, so a render (which begins from a reset chip) reproduces the
    /// timbre, per-channel output (L/R stereo enable + algorithm), and operator
    /// levels the driver programmed BEFORE the capture window opened. Without
    /// this, a driver that sets its voices once during setup — especially the
    /// sole carrier of FM algorithm 0 (slot 4) or the channel stereo-enable bits
    /// — renders silent even though the captured note-on/frequency stream is
    /// correct. Returns `(bank, addr, value)` for every *written* SSG register
    /// (0x00-0x0D, bank 0 only) and FM operator/channel register (0x30-0xB7),
    /// address-ascending so operator setup precedes the per-channel registers;
    /// transient timer/key-on (0x24-0x28) and rhythm/ADPCM (0x10-0x1F) are left
    /// to the captured stream.
    pub fn seed_writes(&self) -> Vec<(u8, u8, u8)> {
        let mut out = Vec::new();
        for bank in 0..2 {
            for addr in 0u8..=0xFF {
                let want = (bank == 0 && (0x00..=0x0D).contains(&addr))
                    || (0x30..=0xB7).contains(&addr);
                if want && self.written[bank][addr as usize] {
                    out.push((bank as u8, addr, self.regs[bank][addr as usize]));
                }
            }
        }
        out
    }

    /// Write to the bank-0 data port (A0=1). Sets `last_write` for the caller to log.
    pub fn write_data(&mut self, v: u8) {
        let addr = self.addr[0];
        self.regs[0][addr as usize] = v;
        self.written[0][addr as usize] = true;
        self.last_write = Some((0, addr, v));
        match addr {
            0x24 | 0x25 => {
                // Period takes effect on next (re)load / overflow reload.
                self.timer_a.period = self.timer_a_period();
            }
            0x26 => {
                self.timer_b.period = self.timer_b_period();
            }
            0x27 => {
                let load_a = v & 0x01 != 0;
                let load_b = v & 0x02 != 0;
                self.timer_a.enabled = v & 0x04 != 0;
                self.timer_b.enabled = v & 0x08 != 0;
                // Refresh the reload period from the current NA/NB. A driver may
                // never write 0x24-0x26 and rely on the reset default (NB=0 →
                // period 16×256), so the `period` field must be current at load
                // time, not just when a period register is written (else it
                // keeps the Timer::new placeholder and mis-reloads on overflow).
                self.timer_a.period = self.timer_a_period();
                self.timer_b.period = self.timer_b_period();
                if load_a && !self.timer_a.running {
                    self.timer_a.counter = self.timer_a.period;
                }
                if load_b && !self.timer_b.running {
                    self.timer_b.counter = self.timer_b.period;
                }
                self.timer_a.running = load_a;
                self.timer_b.running = load_b;
                if v & 0x10 != 0 {
                    self.timer_a.flag = false;
                }
                if v & 0x20 != 0 {
                    self.timer_b.flag = false;
                }
                self.note_irq_edge();
            }
            _ => {}
        }
    }

    /// Status read (A0=0): bit0 = Timer A flag, bit1 = Timer B flag,
    /// bit7 = busy (always 0 here — see crate docs).
    pub fn read_status(&self) -> u8 {
        (self.timer_a.flag as u8) | ((self.timer_b.flag as u8) << 1)
    }

    /// Data read (A0=1): SSG registers 0x00-0x0F read back; register 0xFF reads
    /// as the OPNA identity byte (1) so sound-board detection succeeds; other FM
    /// registers are write-only on real silicon and return 0.
    pub fn read_data(&self) -> u8 {
        if self.addr[0] < 0x10 {
            self.regs[0][self.addr[0] as usize]
        } else if self.addr[0] == 0xFF && self.opna {
            1
        } else {
            0
        }
    }

    /// Mark this instance as an OPNA (YM2608) so register-0xFF reads identify
    /// the Sound Board II. See the `opna` field.
    pub fn set_opna(&mut self, v: bool) {
        self.opna = v;
    }

    /// Current bank-0 address latch (diagnostics + detection readback).
    pub fn addr0(&self) -> u8 {
        self.addr[0]
    }

    /// Current bank-1 address latch.
    pub fn addr1(&self) -> u8 {
        self.addr[1]
    }

    /// Whether OPNA 6-channel ("extended") mode is enabled.
    pub fn extend(&self) -> bool {
        self.extend
    }

    /// Enable/disable OPNA extended mode (driven by the 86-board PCM control bit).
    /// Preset an SSG-region register's readback value without logging it. Used to
    /// satisfy a driver that reads a board-config jumper on an SSG I/O port — the
    /// MUSE drivers read reg 0x0E during INIT to choose their API interrupt
    /// vector. reg 0x0E is outside the seeded range (0x00-0x0D), so this never
    /// affects a capture.
    pub fn set_ssg_readback(&mut self, addr: u8, val: u8) {
        if (addr as usize) < 0x10 {
            self.regs[0][addr as usize] = val;
        }
    }

    pub fn set_extend(&mut self, e: bool) {
        self.extend = e;
    }

    /// IRQ line level (flag & enable is already folded into flag setting).
    pub fn irq(&self) -> bool {
        self.timer_a.flag || self.timer_b.flag
    }

    /// Timer B state for diagnostics: (running, enabled, counter, period, flag).
    pub fn timer_b_debug(&self) -> (bool, bool, u32, u32, bool) {
        let t = &self.timer_b;
        (t.running, t.enabled, t.counter, t.period, t.flag)
    }

    /// Timer A state for diagnostics: (running, enabled, counter, period, flag).
    pub fn timer_a_debug(&self) -> (bool, bool, u32, u32, bool) {
        let t = &self.timer_a;
        (t.running, t.enabled, t.counter, t.period, t.flag)
    }

    /// Clear both timer flags. On PC-88 the sound driver's ISR acknowledges the
    /// interrupt by writing port 0xe4; hoot's mucom88 lowers the IRQ line there
    /// independently of the 0x27 flag-reset, so the harness mirrors that here.
    pub fn ack_irq(&mut self) {
        self.timer_a.flag = false;
        self.timer_b.flag = false;
        self.note_irq_edge();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLOCK: u32 = 3_993_600;

    fn write(opn: &mut Opn, addr: u8, data: u8) {
        opn.write_addr(addr);
        opn.write_data(data);
    }

    #[test]
    fn timer_a_fires_at_documented_rate() {
        let mut opn = Opn::new(CLOCK);
        // NA = 0x3F8 (1016) → period 8 steps = 8×72 = 576 chip clocks
        write(&mut opn, 0x24, 0xFE);
        write(&mut opn, 0x25, 0x00);
        write(&mut opn, 0x27, 0x05); // load A + enable A
        assert_eq!(opn.read_status(), 0);
        opn.tick(575);
        assert_eq!(opn.read_status(), 0);
        opn.tick(1);
        assert_eq!(opn.read_status(), 0x01);
        assert!(opn.irq());
        // reset flag
        write(&mut opn, 0x27, 0x15);
        assert_eq!(opn.read_status(), 0);
        // still running: fires again after another period
        opn.tick(576);
        assert_eq!(opn.read_status(), 0x01);
    }

    #[test]
    fn timer_b_units_of_16() {
        let mut opn = Opn::new(CLOCK);
        write(&mut opn, 0x26, 0xFF); // NB=255 → period 16 steps = 1152 clocks
        write(&mut opn, 0x27, 0x0A); // load B + enable B
        opn.tick(1151);
        assert_eq!(opn.read_status(), 0);
        opn.tick(1);
        assert_eq!(opn.read_status(), 0x02);
    }

    #[test]
    fn disabled_timer_sets_no_flag() {
        let mut opn = Opn::new(CLOCK);
        write(&mut opn, 0x24, 0xFE);
        write(&mut opn, 0x25, 0x00);
        write(&mut opn, 0x27, 0x01); // load A, IRQ disabled
        opn.tick(10_000);
        assert_eq!(opn.read_status(), 0);
        assert!(!opn.irq());
    }

    #[test]
    fn ssg_readback() {
        let mut opn = Opn::new(CLOCK);
        write(&mut opn, 0x07, 0x3E);
        opn.write_addr(0x07);
        assert_eq!(opn.read_data(), 0x3E);
        write(&mut opn, 0x30, 0x71); // FM register: write-only
        opn.write_addr(0x30);
        assert_eq!(opn.read_data(), 0);
    }

    #[test]
    fn prescaler_changes_timer_rate() {
        let mut opn = Opn::new(CLOCK);
        opn.write_addr(0x2F); // prescale 2 → 24 clocks per step
        write(&mut opn, 0x24, 0xFE);
        write(&mut opn, 0x25, 0x00);
        write(&mut opn, 0x27, 0x05);
        opn.tick(8 * 24 - 1);
        assert_eq!(opn.read_status(), 0);
        opn.tick(1);
        assert_eq!(opn.read_status(), 0x01);
    }
}
