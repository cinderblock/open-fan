//! PawnIO-backed hardware access.
//!
//! PawnIO is a signed, scriptable kernel driver: the driver itself is generic and
//! audited, and hardware-specific logic ships as sandboxed Pawn bytecode modules that an
//! application loads at runtime. It is the reason this project does not need — and must
//! never use — the abandoned `WinRing0` shim, which Microsoft Defender now flags as
//! `HackTool:Win32/Winring0` under CVE-2020-14979. See `plans/open-fan.md`.
//!
//! # Licensing
//!
//! We talk to PawnIO exclusively through `PawnIOLib.dll`, loaded dynamically at runtime.
//! `PawnIOLib` is LGPL-2.1, the driver is GPL-2.0 with an explicit exception for
//! independent programs communicating over its device I/O control interface, and the
//! official hardware modules are LGPL-2.1 and distributed pre-signed. Dynamic linking and
//! shipping no PawnIO source keeps this crate, and OpenFan, MIT.
//!
//! **Do not vendor PawnIO source into this repository**, and do not statically link it.
//!
//! # Status
//!
//! Phase 1 scaffold. The FFI surface below is complete and matches `PawnIOLib.h`; the
//! Super I/O discovery, sensor decode and PWM control built on top of it land in Phase 3,
//! once the reference machine is known (see `plans/open-fan.md`, Open Question 1).

#[cfg(windows)]
mod ffi;
#[cfg(windows)]
pub mod isa;
#[cfg(windows)]
pub mod lpc;
#[cfg(windows)]
pub mod nct6775;

#[cfg(windows)]
pub use ffi::{
    PawnIo, PawnIoError, is_available, library_version, module_cache_dir, module_search_dirs,
};

#[cfg(not(windows))]
compile_error!(
    "of-hal-pawnio is Windows-only. Gate it behind `[target.'cfg(windows)'.dependencies]` \
     so Linux and macOS builds can still compile the rest of the workspace."
);
