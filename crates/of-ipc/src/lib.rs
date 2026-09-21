//! Shared types between the OpenFan backend and its UI.
//!
//! TypeScript definitions are generated from these types with `ts-rs` (`cargo test -p
//! of-ipc` writes them), so the frontend cannot drift from the backend's idea of the
//! protocol without the build noticing.
//!
//! # Status
//!
//! Phase 1 scaffold. The command and event vocabulary fills in alongside the Tauri
//! commands in Phase 2.

#![forbid(unsafe_code)]

// Re-exported so the UI's generated bindings and the backend agree on one definition of
// the port type system rather than two.
pub use of_units::{Quantity, Value};
