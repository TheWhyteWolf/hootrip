# Vendored: iz80 0.5.1

Upstream: https://github.com/ivanizag/iz80 (BSD-3-Clause, LICENSE retained).

Vendored 2026-07-23 for hootrip with the following patches (all marked
`HOOTRIP PATCH` in the source):

1. `src/cpu.rs` — HALT no longer blocks interrupt servicing (upstream returned
   early while halted, deadlocking HALT-wait loops); halted CPU burns 4 cycles
   per call so cycle-paced hardware keeps advancing.
2. `src/cpu.rs` — IM 2 implemented (vector table fetch from `I:int_vector`,
   19 cycles) and IM 0 implemented for RST-family bus bytes (upstream panicked
   on both). New `Cpu::set_interrupt_vector()` sets the data-bus byte.
3. `src/state.rs` — added `int_vector` field (not serialized).
4. `src/cpu.rs` / `src/state.rs` — added `int_accepted` flag +
   `Cpu::take_interrupt_accepted()` so hosts can tell whether a signaled
   interrupt was actually serviced (IFF1 inspection is ambiguous when the
   ISR's first instruction is EI).

Upstream tests are retained and must keep passing (`cargo test -p iz80`).
