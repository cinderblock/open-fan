//! The OpenFan service: the process that actually controls the fans.
//!
//! Moving control out of the desktop application and into a service is what lets fan
//! control start at boot before anyone logs in, keep running across logoff, and hold the
//! single elevated handle to the hardware — so the editor can be an ordinary unelevated
//! window that anyone can close.
//!
//! The split is possible because `of-engine` was always a plain library with no opinion
//! about its host. Nothing safety-critical moved to make this work.
//!
//! * [`host`] — the engine and the request handler. No service machinery, so it can be
//!   run as a console process and behave identically.
//! * [`takeover`] — standing rival fan controllers down. Lives here because this is where
//!   the hardware is; the editor asks, the service acts.

pub mod host;
pub mod takeover;

pub use host::{Host, driver_summary, serve};
