//! PC-98 (`pc98dos`) harness: re-hosts MS-DOS sound-driver programs on the
//! vendored NP2 i286c core and logs OPN/OPNA register writes.
//!
//! Unlike the PC-88 harness (which boots a raw driver+data blob), a `pc98dos`
//! set is a genuine DOS program chain: loose `.COM`/`.EXE` files in a virtual
//! working directory, run via a list of shell commands, with the last usually a
//! resident selector stub fed console input (`conin`) to pick a track. See
//! [`dos`] for the minimal DOS environment.

pub mod dos;
pub mod harness;
pub mod io;

pub use harness::{
    rip_title, trace_capture, trace_title, CaptureTrace, Pc98RipOptions, Pc98RipOutcome, ShellStep,
    StepResult, TraceReport, PC98_CPU_HZ, PC98_OPNA_CLOCK_HZ, PC98_OPN_CLOCK_HZ,
};
