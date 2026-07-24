//! hoot-machine: per-platform harnesses that re-host hoot sets' sound-driver
//! code on emulated CPUs and log sound-chip register writes.

pub mod pc88;
pub mod pc98;

pub use pc88::{rip_title, RipOptions, RipOutcome, PC88_CPU_HZ, PC88_OPN_CLOCK_HZ};
