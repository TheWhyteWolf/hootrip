//! Real-mode x86 CPU abstraction for PC-98 sound-driver re-hosting.
//!
//! The concrete core is NP2's (Neko Project II) `i286c` — an 8086 / V30 / 186 /
//! 286 interpreter — vendored under `vendor/np2/` and driven through FFI (see
//! `README-VENDORED.md`). The [`X86Cpu`] trait is the seam the PC-98 harness in
//! `hoot-machine` talks to, so the core stays swappable (e.g. NP2's `i386c` for
//! the handful of sets that use 386 instructions).
//!
//! ## Memory vs I/O
//! The core shares one flat physical memory image (`mem[]` in C). The harness
//! reads and writes it directly through [`X86Cpu::mem`] (to load `.COM`/`.EXE`
//! images, build the PSP, patch the IVT, and service DOS calls). Only port I/O
//! goes through the [`IoBus`] callback, installed for the duration of a
//! [`X86Cpu::run`] batch.
//!
//! ## Trapping DOS / driver calls
//! Software interrupts (INT 21h, the driver `funcvect`, PC-98 INT 60h BIOS) are
//! intercepted by pointing their real-mode IVT entries at a small in-guest
//! trampoline whose first byte is `HLT`. Executing it stops the run batch with
//! [`Stop::Halted`]; the harness inspects CS:IP to tell a trampoline trap from a
//! driver's genuine idle-HLT (which instead gets the pending timer IRQ).

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod mock;
pub mod np2;

/// 16-bit real-mode registers and segment selectors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reg16 {
    Ax,
    Bx,
    Cx,
    Dx,
    Si,
    Di,
    Bp,
    Sp,
    Cs,
    Ds,
    Es,
    Ss,
    Ip,
    Flags,
}

/// FLAGS register bits (8086 / 286).
pub mod flag {
    pub const CF: u16 = 0x0001;
    pub const PF: u16 = 0x0004;
    pub const AF: u16 = 0x0010;
    pub const ZF: u16 = 0x0040;
    pub const SF: u16 = 0x0080;
    pub const TF: u16 = 0x0100;
    pub const IF: u16 = 0x0200;
    pub const DF: u16 = 0x0400;
    pub const OF: u16 = 0x0800;
}

/// Why a [`X86Cpu::run`] batch stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stop {
    /// The clock budget was exhausted.
    Clocks,
    /// The CPU executed `HLT`. The harness reads CS:IP to distinguish an
    /// interception trampoline from a driver's genuine wait-for-interrupt idle.
    Halted,
}

/// Port-I/O dispatch. Memory is deliberately absent: it is the CPU's shared
/// flat image, accessed via [`X86Cpu::mem`].
pub trait IoBus {
    fn out8(&mut self, port: u16, val: u8);
    fn in8(&mut self, port: u16) -> u8;
    fn out16(&mut self, port: u16, val: u16) {
        self.out8(port, val as u8);
        self.out8(port.wrapping_add(1), (val >> 8) as u8);
    }
    fn in16(&mut self, port: u16) -> u16 {
        self.in8(port) as u16 | ((self.in8(port.wrapping_add(1)) as u16) << 8)
    }
}

/// A real-mode x86 core the PC-98 harness drives.
pub trait X86Cpu {
    /// Power-on reset (registers, flags, segment bases).
    fn reset(&mut self);

    /// The shared flat physical memory image. Real-mode linear address is
    /// `segment * 16 + offset`; the harness loads programs and edits the IVT
    /// through this slice.
    fn mem(&mut self) -> &mut [u8];
    /// Read-only view of the same image.
    fn mem_ref(&self) -> &[u8];

    fn reg16(&self, r: Reg16) -> u16;
    fn set_reg16(&mut self, r: Reg16, v: u16);

    /// Set CS:IP together, recomputing the internal code-segment base.
    fn set_cs_ip(&mut self, cs: u16, ip: u16);
    /// Set SS:SP together, recomputing the internal stack-segment base.
    fn set_ss_sp(&mut self, ss: u16, sp: u16);

    /// Deliver an interrupt exactly as the CPU would: push FLAGS/CS/IP, vector
    /// through the real-mode IVT at `[vector*4]`, clear IF and TF. Used for the
    /// PC-98 timer IRQ that paces the sound driver.
    fn interrupt(&mut self, vector: u8);

    /// Whether the most recent [`run`](X86Cpu::run) ended on `HLT`.
    fn halted(&self) -> bool;

    /// Real-mode address mask (8086 / V30 = `0x000F_FFFF`, 286 = `0x00FF_FFFF`).
    fn set_adrsmask(&mut self, mask: u32);
    /// Enable NEC V30 opcode semantics (vs Intel 286).
    fn set_v30(&mut self, on: bool);

    /// Execute up to `clocks` CPU cycles against `io`, returning early on `HLT`.
    /// Returns the cycles actually consumed and the stop reason.
    fn run(&mut self, io: &mut dyn IoBus, clocks: u32) -> (u32, Stop);
}
