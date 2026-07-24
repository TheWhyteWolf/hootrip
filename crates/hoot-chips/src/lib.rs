//! Chip front-ends for register logging.
//!
//! A ripper does not need audio synthesis — it needs the *bus-visible*
//! behaviour drivers depend on: address latching, status flags, timer
//! overflow/IRQ timing, and readable registers. Audio rendering of the
//! resulting logs is done by external players (libvgm etc.).

pub mod opn;

pub use opn::Opn;
