//! The OpenFan control engine.
//!
//! Owns the tick loop, the hardware backend, and — most importantly — the safety
//! supervisor. The layered safety design is specified in `plans/open-fan.md`.
//!
//! The crate is split so that the part which must be *correct* is separable from the
//! part which must be *timely*:
//!
//! - [`policy`] decides what to write, given an evaluated tick. Pure.
//! - [`engine`] owns the backend and the graph and performs one tick when told to. It
//!   takes `dt` as an argument and reads no clock, so every behaviour — including
//!   recovery from a sensor dropping out mid-run — is reproducible in a test.
//! - [`runner`] is the only part that touches wall-clock time and threads.
//!
//! Anything that looks like a control decision belongs in the first two.
//!
//! # Status
//!
//! Phase 2. The tick loop and its fault handling are implemented. The watchdog thread,
//! external watchdog process, crash reporting and auto-restart land in Phase 4.

#![forbid(unsafe_code)]

pub mod engine;
pub mod policy;
pub mod runner;

pub use engine::{Engine, EngineConfig, SensorSnapshot, TickReport};
pub use policy::{Applied, ChannelPolicy, FailsafeAction, SafetyPolicy, apply_tick, dying_breath};
pub use runner::{EngineHandle, Snapshot};
