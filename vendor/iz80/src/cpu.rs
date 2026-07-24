use std::io;

use super::decoder_z80::DecoderZ80;
use super::decoder_8080::Decoder8080;
use super::environment::Environment;
use super::machine::Machine;
use super::opcode::Opcode;
use super::registers::{Reg16, Reg8, Registers};
use super::state::State;

const IRQ_ADDRESS: u16 = 0x0038;
const NMI_ADDRESS: u16 = 0x0066;

/// The Z80 cpu emulator.
///
/// Executes Z80 instructions changing the cpu State and Machine
pub struct Cpu {
    state: State,
    trace: bool,
    decoder: Box<dyn Decoder + Send + Sync>,
}

// Ensure that the Cpu is Send and Sync and can be used with async code
const _: () = {
    fn assert_send<T: Send + Sync>() {}
    let _ = assert_send::<Cpu>;
};

pub(crate) trait Decoder {
    fn decode(&self, env: &mut Environment) -> &Opcode;
}

impl Cpu {

    /// Returns a Z80 Cpu instance. Alias of `new_z80()`
    pub fn new() -> Cpu {
        Self::new_z80()
    }

    /// Returns a Z80 Cpu instance
    pub fn new_z80() -> Cpu {
        Cpu {
            state: State::new(),
            trace: false,
            decoder: Box::new(DecoderZ80::new())
        }
    }

    /// Returns an Intel 8080 Cpu instance
    pub fn new_8080() -> Cpu {
        let mut cpu = Cpu {
            state: State::new(),
            trace: false,
            decoder: Box::new(Decoder8080::new())
        };

        cpu.state.reg.set_8080();
        cpu
    }

}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

impl Cpu {
    /// Executes a single instruction
    ///
    /// # Arguments
    ///
    /// * `sys` - A representation of the emulated machine that has the Machine trait
    ///
    pub fn execute_instruction(&mut self, sys: &mut dyn Machine) {
        if self.is_halted() {
            // HOOTRIP PATCH: a pending reset/NMI/serviceable INT must wake the
            // CPU (upstream returned unconditionally, deadlocking HALT loops).
            // While staying halted, still burn cycles so emulated time
            // advances for callers that pace hardware off cycle_count().
            let int_serviceable =
                self.state.int_signaled && self.state.reg.get_interrupt_mode().0
                    && !self.state.int_just_enabled;
            if !(self.state.reset_pending || self.state.nmi_pending || int_serviceable) {
                self.state.cycle = self.state.cycle.wrapping_add(4);
                return
            }
        }

        let mut env = Environment::new(&mut self.state, sys);
        if env.state.reset_pending {
            env.state.reset_pending = false;
            env.state.nmi_pending = false;
            env.state.halted = false;
            env.state.reg.reset();
            env.state.cycle = env.state.cycle.wrapping_add(3);
        }
        else if env.state.nmi_pending {
            env.state.nmi_pending = false;
            env.state.halted = false;
            env.state.reg.start_nmi();
            env.state.cycle = env.state.cycle.wrapping_add(11);
            env.subroutine_call(NMI_ADDRESS);
        } else if env.state.int_signaled {
            let (int_enabled, int_mode) = env.state.reg.get_interrupt_mode();
            if int_enabled && !env.state.int_just_enabled {
                env.state.halted = false;
                env.state.reg.set_interrupts(false);
                env.state.int_accepted = true; // HOOTRIP PATCH
                match int_mode {
                    // HOOTRIP PATCH: IM 0 services RST-family bus bytes only —
                    // the sole pattern used by the machines we emulate.
                    0 => {
                        let bus = env.state.int_vector;
                        if bus & 0xC7 == 0xC7 {
                            env.state.cycle = env.state.cycle.wrapping_add(13);
                            env.subroutine_call((bus & 0x38) as u16);
                        }
                    },
                    1 => {
                        env.state.cycle = env.state.cycle.wrapping_add(13);
                        env.subroutine_call(IRQ_ADDRESS);
                    },
                    // HOOTRIP PATCH: IM 2 — vector table entry at I:int_vector.
                    2 => {
                        env.state.cycle = env.state.cycle.wrapping_add(19);
                        let table = ((env.state.reg.get8(Reg8::I) as u16) << 8)
                            | (env.state.int_vector as u16);
                        let target = env.sys.peek16(table);
                        env.subroutine_call(target);
                    },
                    _ => panic!("Invalid interrupt mode")
                }
            }
        }

        let pc = env.state.reg.pc();
        let opcode = self.decoder.decode(&mut env);
        if self.trace {
            print!("==> {:04x}: {:20}", pc, opcode.disasm(&mut env));
        }

        env.clear_branch_taken();
        env.clear_int_just_enabled();
        opcode.execute(&mut env);
        env.advance_cycles(opcode);
        env.clear_index();

        if self.trace {
            print!(" PC:{:04x} AF:{:04x} BC:{:04x} DE:{:04x} HL:{:04x} SP:{:04x} IX:{:04x} IY:{:04x} Flags:{:08b} Cycle:{:04}",
                self.state.reg.pc(),
                self.state.reg.get16(Reg16::AF),
                self.state.reg.get16(Reg16::BC),
                self.state.reg.get16(Reg16::DE),
                self.state.reg.get16(Reg16::HL),
                self.state.reg.get16(Reg16::SP),
                self.state.reg.get16(Reg16::IX),
                self.state.reg.get16(Reg16::IY),
                self.state.reg.get8(Reg8::F),
                self.state.cycle
            );
            println!(" [{:02x} {:02x} {:02x}]", sys.peek(pc),
                sys.peek(pc.wrapping_add(1)), sys.peek(pc.wrapping_add(2)));
        }
    }

    /// Returns the instruction in PC disassembled. PC is advanced.
    ///
    /// # Arguments
    ///
    /// * `sys` - A representation of the emulated machine that has the Machine trait
    ///
    pub fn disasm_instruction(&mut self, sys: &mut dyn Machine) -> String {
        let mut env = Environment::new(&mut self.state, sys);
        let opcode = self.decoder.decode(&mut env);
        opcode.disasm(&mut env)
    }

    /// Activates or deactivates traces of the instruction executed and
    /// the state of the registers.
    ///
    /// # Arguments
    ///
    /// * `trace` - A bool defining the trace state to set
    pub fn set_trace(&mut self, trace: bool) {
        self.trace = trace;
    }

    /// Returns a Registers struct to read and write on the Z80 registers
    pub fn registers(&mut self) -> &mut Registers {
        &mut self.state.reg
    }

    /// Returns an immutable references to Registers struct to read the Z80 registers
    pub fn immutable_registers(&self) -> &Registers {
        &self.state.reg
    }

    /// Returns if the Cpu has executed a HALT
    pub fn is_halted(&self) -> bool {
        self.state.halted
            && !self.state.nmi_pending
            && !self.state.reset_pending
            && !self.state.int_signaled
    }

    /// Maskable interrupt request. It stays signaled until is is
    /// deactivated by calling `signal_interrupt(false)`.
    /// HOOTRIP PATCH: sets the byte the interrupting device places on the
    /// data bus — the low vector byte in IM 2, the opcode (RST family only)
    /// in IM 0. Ignored in IM 1.
    pub fn set_interrupt_vector(&mut self, vector: u8) {
        self.state.int_vector = vector;
    }

    /// HOOTRIP PATCH: returns whether a maskable interrupt was serviced since
    /// the last call, clearing the flag.
    pub fn take_interrupt_accepted(&mut self) -> bool {
        let v = self.state.int_accepted;
        self.state.int_accepted = false;
        v
    }

    pub fn signal_interrupt(&mut self, active: bool) {
        self.state.int_signaled = active;
    }

    /// Non maskable interrupt request
    pub fn signal_nmi(&mut self) {
        self.state.nmi_pending = true;
    }

    /// Signal reset
    pub fn signal_reset(&mut self) {
        self.state.reset_pending = true;
    }

    /// Returns the current cycle count
    pub fn cycle_count(&self) -> u64 {
        self.state.cycle
    }

    // Serialize the current state of the CPU
    pub fn serialize(&self) -> Vec<u8> {
        self.state.serialize()
    }

    // Update the CPU state from a serialized state
    pub fn deserialize(&mut self, data: &[u8]) -> io::Result<()> {
        self.state.deserialize(data)
    }
}
