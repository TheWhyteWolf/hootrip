//! FFI wrapper around the vendored NP2 `i286c` core (`vendor/np2/`), built by
//! `build.rs`. Implements [`X86Cpu`](crate::X86Cpu).
//!
//! The core keeps its state in C globals (`i286core`, `mem[]`), so [`Np2Cpu`]
//! is a singleton handle over that global state — constructing two at once
//! panics. Port I/O is routed into a caller-supplied [`IoBus`](crate::IoBus)
//! for the duration of each [`run`](Np2Cpu::run) via a thread-local pointer and
//! the C hook function pointers in `np2io.c`.

use std::cell::Cell;
use std::os::raw::{c_int, c_uchar, c_uint, c_ushort};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{IoBus, Reg16, Stop, X86Cpu};

/// Physical memory size (matches `mem[0x200000]` in the C core).
pub const MEM_SIZE: usize = 0x20_0000;

const HLT: u8 = 0xF4;

extern "C" {
    fn np2_init();
    fn np2_reset();
    fn np2_setextsize(sz: c_uint);
    fn np2_reg_get(idx: c_int) -> c_ushort;
    fn np2_reg_set(idx: c_int, v: c_ushort);
    fn np2_set_cs_ip(cs: c_ushort, ip: c_ushort);
    fn np2_set_ss_sp(ss: c_ushort, sp: c_ushort);
    fn np2_set_adrsmask(m: c_uint);
    fn np2_set_v30(on: c_int);
    fn np2_interrupt(vect: c_uchar);
    fn np2_step() -> c_int;
    fn np2_pc_phys() -> c_uint;
    fn np2_mem() -> *mut u8;

    static mut hootrip_out8: Option<extern "C" fn(c_uint, c_uchar)>;
    static mut hootrip_out16: Option<extern "C" fn(c_uint, c_ushort)>;
    static mut hootrip_inp8: Option<extern "C" fn(c_uint) -> c_uchar>;
    static mut hootrip_inp16: Option<extern "C" fn(c_uint) -> c_ushort>;
}

fn reg_index(r: Reg16) -> c_int {
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

thread_local! {
    /// Non-null only for the duration of a `run` call. The pointee outlives the
    /// synchronous C step that reads it, so dereferencing in the trampolines is
    /// sound.
    static IO_PTR: Cell<Option<*mut (dyn IoBus + 'static)>> = const { Cell::new(None) };
}

extern "C" fn tramp_out8(port: c_uint, val: c_uchar) {
    IO_PTR.with(|c| {
        if let Some(p) = c.get() {
            unsafe { (*p).out8(port as u16, val as u8) }
        }
    });
}
extern "C" fn tramp_out16(port: c_uint, val: c_ushort) {
    IO_PTR.with(|c| {
        if let Some(p) = c.get() {
            unsafe { (*p).out16(port as u16, val as u16) }
        }
    });
}
extern "C" fn tramp_in8(port: c_uint) -> c_uchar {
    IO_PTR.with(|c| match c.get() {
        Some(p) => unsafe { (*p).in8(port as u16) },
        None => 0xFF,
    })
}
extern "C" fn tramp_in16(port: c_uint) -> c_ushort {
    IO_PTR.with(|c| match c.get() {
        Some(p) => unsafe { (*p).in16(port as u16) },
        None => 0xFFFF,
    })
}

static IN_USE: AtomicBool = AtomicBool::new(false);

/// Handle to the vendored NP2 i286c core (singleton over its C globals).
pub struct Np2Cpu {
    halted: bool,
}

impl Np2Cpu {
    /// Bring the core up (`i286c_initialize` + `i286c_reset`). Panics if another
    /// [`Np2Cpu`] is already live — the core's state is a process global.
    pub fn new() -> Self {
        if IN_USE.swap(true, Ordering::SeqCst) {
            panic!("Np2Cpu is a singleton: only one may exist at a time");
        }
        unsafe {
            np2_init();
            np2_reset();
            np2_setextsize(0);
        }
        Np2Cpu { halted: false }
    }
}

impl Default for Np2Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Np2Cpu {
    fn drop(&mut self) {
        IN_USE.store(false, Ordering::SeqCst);
    }
}

impl X86Cpu for Np2Cpu {
    fn reset(&mut self) {
        unsafe { np2_reset() };
        self.halted = false;
    }

    fn mem(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(np2_mem(), MEM_SIZE) }
    }
    fn mem_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(np2_mem(), MEM_SIZE) }
    }

    fn reg16(&self, r: Reg16) -> u16 {
        unsafe { np2_reg_get(reg_index(r)) }
    }
    fn set_reg16(&mut self, r: Reg16, v: u16) {
        unsafe { np2_reg_set(reg_index(r), v) }
    }

    fn set_cs_ip(&mut self, cs: u16, ip: u16) {
        unsafe { np2_set_cs_ip(cs, ip) }
    }
    fn set_ss_sp(&mut self, ss: u16, sp: u16) {
        unsafe { np2_set_ss_sp(ss, sp) }
    }

    fn interrupt(&mut self, vector: u8) {
        unsafe { np2_interrupt(vector) };
        self.halted = false;
    }

    fn halted(&self) -> bool {
        self.halted
    }

    fn set_adrsmask(&mut self, mask: u32) {
        unsafe { np2_set_adrsmask(mask) }
    }
    fn set_v30(&mut self, on: bool) {
        unsafe { np2_set_v30(on as c_int) }
    }

    fn run(&mut self, io: &mut dyn IoBus, clocks: u32) -> (u32, Stop) {
        self.halted = false;
        // Erase the borrow lifetime for the duration of the synchronous C run.
        // Sound: `io` outlives every C step that reads the thread-local, and the
        // pointer is cleared before `run` returns.
        let io_ptr: *mut (dyn IoBus + 'static) =
            unsafe { std::mem::transmute::<*mut dyn IoBus, *mut (dyn IoBus + 'static)>(io) };
        IO_PTR.with(|c| c.set(Some(io_ptr)));
        unsafe {
            hootrip_out8 = Some(tramp_out8);
            hootrip_out16 = Some(tramp_out16);
            hootrip_inp8 = Some(tramp_in8);
            hootrip_inp16 = Some(tramp_in16);
        }

        let mem = unsafe { np2_mem() };
        let mut consumed: u32 = 0;
        let stop = loop {
            let phys = unsafe { np2_pc_phys() } as usize;
            if unsafe { *mem.add(phys & (MEM_SIZE - 1)) } == HLT {
                self.halted = true;
                break Stop::Halted;
            }
            if consumed >= clocks {
                break Stop::Clocks;
            }
            let c = unsafe { np2_step() };
            consumed = consumed.saturating_add(c.max(0) as u32);
        };

        unsafe {
            hootrip_out8 = None;
            hootrip_out16 = None;
            hootrip_inp8 = None;
            hootrip_inp16 = None;
        }
        IO_PTR.with(|c| c.set(None));
        (consumed, stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captures port writes so a test can assert the driver hit the chip.
    #[derive(Default)]
    struct RecIo {
        outs: Vec<(u16, u8)>,
    }
    impl IoBus for RecIo {
        fn out8(&mut self, port: u16, val: u8) {
            self.outs.push((port, val));
        }
        fn in8(&mut self, _port: u16) -> u8 {
            0xFF
        }
    }

    // One test only: the core is a process global, and cargo runs tests in
    // parallel threads, so a second `Np2Cpu::new()` would hit the singleton
    // guard. Exercise both behaviours sequentially on one instance.
    #[test]
    fn executes_realmode_traps_hlt_and_interrupts() {
        let mut cpu = Np2Cpu::new();
        cpu.set_adrsmask(0x000F_FFFF);
        // Program at CS=0x1000 (linear 0x10000):
        //   B8 34 12  MOV AX,0x1234
        //   89 C3     MOV BX,AX
        //   01 D8     ADD AX,BX          ; AX=0x2468
        //   C1 EB 04  SHR BX,4           ; BX=0x0123  (286-only)
        //   E6 44     OUT 0x44,AL        ; port write -> IoBus
        //   F4        HLT
        let prog = [
            0xB8, 0x34, 0x12, 0x89, 0xC3, 0x01, 0xD8, 0xC1, 0xEB, 0x04, 0xE6, 0x44, 0xF4,
        ];
        cpu.mem()[0x10000..0x10000 + prog.len()].copy_from_slice(&prog);
        cpu.set_cs_ip(0x1000, 0x0000);
        cpu.set_ss_sp(0x2000, 0xFFFE);

        let mut io = RecIo::default();
        let (cycles, stop) = cpu.run(&mut io, 1000);

        assert_eq!(stop, Stop::Halted, "should stop on HLT");
        assert!(cpu.halted());
        assert_eq!(cpu.reg16(Reg16::Ax), 0x2468);
        assert_eq!(cpu.reg16(Reg16::Bx), 0x0123);
        assert!(cycles > 0);
        // AL was 0x68 after ADD; OUT 0x44,AL must have reached the bus.
        assert_eq!(io.outs, vec![(0x44, 0x68)]);

        // ---- interrupt vectoring through the real-mode IVT ----
        cpu.reset();
        cpu.set_adrsmask(0x000F_FFFF);
        cpu.set_ss_sp(0x2000, 0x0100);
        cpu.set_cs_ip(0x3000, 0x0000);
        let m = cpu.mem();
        m[0x60 * 4] = 0x34; // IVT[0x60] -> 0xF000:0x1234
        m[0x60 * 4 + 1] = 0x12;
        m[0x60 * 4 + 2] = 0x00;
        m[0x60 * 4 + 3] = 0xF0;
        cpu.interrupt(0x60);
        assert_eq!(cpu.reg16(Reg16::Cs), 0xF000);
        assert_eq!(cpu.reg16(Reg16::Ip), 0x1234);
    }
}
