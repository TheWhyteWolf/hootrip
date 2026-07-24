//! A non-executing [`X86Cpu`] used to unit-test the harness's DOS services.
//!
//! It faithfully models register/memory state and real-mode interrupt dispatch
//! (the parts the mini-DOS logic drives directly) but does **not** decode or
//! execute instructions — [`run`](MockCpu::run) is a no-op. It lets loaders,
//! MCB/PSP setup, IVT edits, and INT 21h handlers be tested without the C core.

use crate::{IoBus, Reg16, Stop, X86Cpu};

/// Physical memory size mirrored from the NP2 core (`mem[0x200000]`).
pub const MEM_SIZE: usize = 0x20_0000;

pub struct MockCpu {
    mem: Vec<u8>,
    r: [u16; 14], // indexed by Reg16 discriminant order
    adrsmask: u32,
    v30: bool,
    halted: bool,
}

fn idx(r: Reg16) -> usize {
    match r {
        Reg16::Ax => 0,
        Reg16::Bx => 1,
        Reg16::Cx => 2,
        Reg16::Dx => 3,
        Reg16::Si => 4,
        Reg16::Di => 5,
        Reg16::Bp => 6,
        Reg16::Sp => 7,
        Reg16::Cs => 8,
        Reg16::Ds => 9,
        Reg16::Es => 10,
        Reg16::Ss => 11,
        Reg16::Ip => 12,
        Reg16::Flags => 13,
    }
}

impl Default for MockCpu {
    fn default() -> Self {
        MockCpu {
            mem: vec![0u8; MEM_SIZE],
            r: [0; 14],
            adrsmask: 0x000F_FFFF,
            v30: false,
            halted: false,
        }
    }
}

impl MockCpu {
    pub fn new() -> Self {
        Self::default()
    }

    fn push16(&mut self, v: u16) {
        let sp = self.reg16(Reg16::Sp).wrapping_sub(2);
        self.set_reg16(Reg16::Sp, sp);
        let lin = ((self.reg16(Reg16::Ss) as usize) << 4).wrapping_add(sp as usize) & (MEM_SIZE - 1);
        self.mem[lin] = v as u8;
        self.mem[(lin + 1) & (MEM_SIZE - 1)] = (v >> 8) as u8;
    }

    fn read16(&self, lin: usize) -> u16 {
        self.mem[lin & (MEM_SIZE - 1)] as u16
            | ((self.mem[(lin + 1) & (MEM_SIZE - 1)] as u16) << 8)
    }
}

impl X86Cpu for MockCpu {
    fn reset(&mut self) {
        self.r = [0; 14];
        self.halted = false;
        // Real-mode reset vector FFFF:0000.
        self.set_reg16(Reg16::Cs, 0xFFFF);
        self.set_reg16(Reg16::Flags, 0x0002);
    }

    fn mem(&mut self) -> &mut [u8] {
        &mut self.mem
    }
    fn mem_ref(&self) -> &[u8] {
        &self.mem
    }

    fn reg16(&self, r: Reg16) -> u16 {
        self.r[idx(r)]
    }
    fn set_reg16(&mut self, r: Reg16, v: u16) {
        self.r[idx(r)] = v;
    }

    fn set_cs_ip(&mut self, cs: u16, ip: u16) {
        self.set_reg16(Reg16::Cs, cs);
        self.set_reg16(Reg16::Ip, ip);
    }
    fn set_ss_sp(&mut self, ss: u16, sp: u16) {
        self.set_reg16(Reg16::Ss, ss);
        self.set_reg16(Reg16::Sp, sp);
    }

    fn interrupt(&mut self, vector: u8) {
        let flags = self.reg16(Reg16::Flags);
        let cs = self.reg16(Reg16::Cs);
        let ip = self.reg16(Reg16::Ip);
        self.push16(flags);
        self.push16(cs);
        self.push16(ip);
        // Vector through the real-mode IVT at [vector*4] = offset, [vector*4+2] = seg.
        let base = (vector as usize) * 4;
        let new_ip = self.read16(base);
        let new_cs = self.read16(base + 2);
        self.set_cs_ip(new_cs, new_ip);
        // Clear IF and TF.
        self.set_reg16(Reg16::Flags, flags & !(crate::flag::IF | crate::flag::TF));
        self.halted = false;
    }

    fn halted(&self) -> bool {
        self.halted
    }

    fn set_adrsmask(&mut self, mask: u32) {
        self.adrsmask = mask;
    }
    fn set_v30(&mut self, on: bool) {
        self.v30 = on;
    }

    /// No-op: the mock does not execute instructions.
    fn run(&mut self, _io: &mut dyn IoBus, _clocks: u32) -> (u32, Stop) {
        (0, Stop::Clocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flag;

    #[test]
    fn interrupt_pushes_frame_and_vectors_via_ivt() {
        let mut cpu = MockCpu::new();
        cpu.set_ss_sp(0x0000, 0x0100);
        cpu.set_cs_ip(0x1000, 0x0042);
        cpu.set_reg16(Reg16::Flags, 0x0202);
        // IVT[0x21] -> 0xF000:0x1234
        cpu.mem()[0x21 * 4] = 0x34;
        cpu.mem()[0x21 * 4 + 1] = 0x12;
        cpu.mem()[0x21 * 4 + 2] = 0x00;
        cpu.mem()[0x21 * 4 + 3] = 0xF0;

        cpu.interrupt(0x21);

        assert_eq!(cpu.reg16(Reg16::Cs), 0xF000);
        assert_eq!(cpu.reg16(Reg16::Ip), 0x1234);
        assert_eq!(cpu.reg16(Reg16::Sp), 0x0100 - 6);
        // IF/TF cleared.
        assert_eq!(cpu.reg16(Reg16::Flags) & flag::IF, 0);
        // Return frame on the stack: IP at top, then CS, then FLAGS.
        assert_eq!(cpu.read16(0x0100 - 2), 0x0202); // flags
        assert_eq!(cpu.read16(0x0100 - 4), 0x1000); // cs
        assert_eq!(cpu.read16(0x0100 - 6), 0x0042); // ip
    }
}
