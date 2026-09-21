//! Raw bindings to `PawnIOLib.dll`.
//!
//! The library is resolved with `LoadLibrary` semantics at runtime rather than linked,
//! because PawnIO is a user-installed prerequisite: OpenFan has to start, explain itself
//! and run in a degraded read-only mode when it is absent, not fail to launch.
//!
//! The C surface, from `PawnIOLib.h`, every function returning an `HRESULT`:
//!
//! ```c
//! HRESULT pawnio_version(PULONG version);
//! HRESULT pawnio_open(PHANDLE handle);
//! HRESULT pawnio_load(HANDLE handle, const UCHAR* blob, SIZE_T size);
//! HRESULT pawnio_execute(HANDLE handle, PCSTR name,
//!                        const ULONG64* in,  SIZE_T in_size,
//!                        ULONG64* out, SIZE_T out_size, PSIZE_T return_size);
//! HRESULT pawnio_close(HANDLE handle);
//! ```

use std::ffi::{CString, c_void};
use std::path::Path;

type Handle = *mut c_void;
type Hresult = i32;

// Signatures as exported. `extern "system"` is stdcall on x86 and the standard C
// convention on x86-64/aarch64, matching STDAPICALLTYPE.
type FnVersion = unsafe extern "system" fn(*mut u32) -> Hresult;
type FnOpen = unsafe extern "system" fn(*mut Handle) -> Hresult;
type FnLoad = unsafe extern "system" fn(Handle, *const u8, usize) -> Hresult;
type FnExecute = unsafe extern "system" fn(
    Handle,
    *const i8,
    *const u64,
    usize,
    *mut u64,
    usize,
    *mut usize,
) -> Hresult;
type FnClose = unsafe extern "system" fn(Handle) -> Hresult;

/// Everything that can go wrong talking to PawnIO.
#[derive(Debug, thiserror::Error)]
pub enum PawnIoError {
    #[error(
        "PawnIO is not installed. OpenFan needs it for hardware access; \
         install it from https://pawnio.eu and restart."
    )]
    NotInstalled,

    #[error("PawnIOLib is missing the {0} export; the installed version may be too old")]
    MissingExport(&'static str),

    #[error("PawnIO call {call} failed (HRESULT 0x{hresult:08X})")]
    Call {
        call: &'static str,
        hresult: Hresult,
    },

    #[error(
        "PawnIO refused to load module {module}. The signed edition only loads modules \
         signed by the PawnIO project; either use an official module or install the \
         Unrestricted edition."
    )]
    ModuleRejected { module: String },

    #[error("function name {0:?} is not valid for FFI")]
    InvalidName(String),
}

type Result<T> = std::result::Result<T, PawnIoError>;

fn check(call: &'static str, hr: Hresult) -> Result<()> {
    if hr >= 0 {
        Ok(())
    } else {
        Err(PawnIoError::Call { call, hresult: hr })
    }
}

struct Lib {
    _library: libloading::Library,
    version: FnVersion,
    open: FnOpen,
    load: FnLoad,
    execute: FnExecute,
    close: FnClose,
}

impl Lib {
    fn load() -> Result<Self> {
        // SAFETY: loading a library runs its initializers. PawnIOLib is a well-behaved
        // installed component; there is no safer alternative to LoadLibrary here.
        let library = unsafe { libloading::Library::new("PawnIOLib.dll") }
            .map_err(|_| PawnIoError::NotInstalled)?;

        // SAFETY: each symbol's type matches the declaration in PawnIOLib.h, quoted in
        // the module docs. The symbols are leaked into 'static lifetimes, which is sound
        // because `_library` is kept alive in the same struct and never unloaded.
        unsafe {
            macro_rules! sym {
                ($name:literal, $ty:ty) => {{
                    let s: libloading::Symbol<$ty> =
                        library
                            .get(concat!($name, "\0").as_bytes())
                            .map_err(|_| PawnIoError::MissingExport($name))?;
                    *s
                }};
            }

            let version = sym!("pawnio_version", FnVersion);
            let open = sym!("pawnio_open", FnOpen);
            let load = sym!("pawnio_load", FnLoad);
            let execute = sym!("pawnio_execute", FnExecute);
            let close = sym!("pawnio_close", FnClose);

            Ok(Self {
                _library: library,
                version,
                open,
                load,
                execute,
                close,
            })
        }
    }
}

/// Whether PawnIO is present on this machine.
///
/// Cheap enough for a first-run check; do not call it every tick.
pub fn is_available() -> bool {
    Lib::load().is_ok()
}

/// PawnIOLib's version as `(major, minor, patch)`.
pub fn library_version() -> Result<(u16, u8, u8)> {
    let lib = Lib::load()?;
    let mut raw: u32 = 0;
    // SAFETY: `raw` is a valid, aligned, initialized u32 for the duration of the call.
    check("pawnio_version", unsafe { (lib.version)(&mut raw) })?;
    Ok((
        ((raw >> 16) & 0xFFFF) as u16,
        ((raw >> 8) & 0xFF) as u8,
        (raw & 0xFF) as u8,
    ))
}

/// An open PawnIO executor with one module loaded.
///
/// Each executor holds a single module, so a backend that needs Super I/O access and MSR
/// access opens two.
pub struct PawnIo {
    lib: Lib,
    handle: Handle,
    module: String,
}

impl PawnIo {
    /// Open an executor and load a module blob into it.
    ///
    /// `module_name` is used only for diagnostics. The blob is an official signed module
    /// from the PawnIO project — see the crate docs on why we never ship our own.
    pub fn load_module(module_name: impl Into<String>, blob: &[u8]) -> Result<Self> {
        let lib = Lib::load()?;
        let module = module_name.into();

        let mut handle: Handle = std::ptr::null_mut();
        // SAFETY: `handle` is a valid out-pointer; the driver writes a handle or leaves
        // it null and returns a failing HRESULT.
        check("pawnio_open", unsafe { (lib.open)(&mut handle) })?;

        let mut this = Self {
            lib,
            handle,
            module,
        };

        // SAFETY: `blob` is a valid readable slice for `blob.len()` bytes, and `handle`
        // came from a successful `pawnio_open`.
        let hr = unsafe { (this.lib.load)(this.handle, blob.as_ptr(), blob.len()) };
        if hr < 0 {
            // Give the signature case its own message: it is the failure users hit, and
            // "HRESULT 0x80070005" tells them nothing about what to do next.
            return Err(PawnIoError::ModuleRejected {
                module: std::mem::take(&mut this.module),
            });
        }

        Ok(this)
    }

    /// Call a function in the loaded module.
    ///
    /// Returns the number of `u64` entries written into `out`.
    pub fn execute(&self, name: &str, input: &[u64], out: &mut [u64]) -> Result<usize> {
        let cname = CString::new(name).map_err(|_| PawnIoError::InvalidName(name.to_owned()))?;
        let mut written: usize = 0;

        // SAFETY: `cname` is NUL-terminated and outlives the call; `input` and `out` are
        // valid slices whose lengths are passed alongside them; `written` is a valid
        // out-pointer. `self.handle` is live until `Drop`.
        let hr = unsafe {
            (self.lib.execute)(
                self.handle,
                cname.as_ptr(),
                input.as_ptr(),
                input.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut written,
            )
        };
        check("pawnio_execute", hr)?;
        Ok(written)
    }

    pub fn module_name(&self) -> &str {
        &self.module
    }

    /// Read an official signed module from disk and load it.
    pub fn load_module_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let blob = std::fs::read(path).map_err(|_| PawnIoError::NotInstalled)?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::load_module(name, &blob)
    }
}

impl Drop for PawnIo {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: the handle came from `pawnio_open` and has not been closed; we
            // null it immediately so a double-close is impossible.
            unsafe { (self.lib.close)(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }
}

// The handle is an opaque kernel object with no thread affinity, and every call takes it
// by value. Moving an executor between threads is fine; sharing it is not, because the
// driver serializes per-handle and `&self` methods would interleave.
unsafe impl Send for PawnIo {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_pawnio_is_reported_as_not_installed_not_a_panic() {
        // Runs on CI machines with no PawnIO. Either outcome is acceptable; what must
        // never happen is a panic or a hang when the driver is missing.
        match library_version() {
            Ok((major, _, _)) => assert!(major < 100, "implausible version"),
            Err(e) => assert!(
                matches!(e, PawnIoError::NotInstalled | PawnIoError::MissingExport(_)),
                "unexpected error kind: {e}"
            ),
        }
    }

    #[test]
    fn availability_check_is_total() {
        let _ = is_available();
    }

    #[test]
    fn failing_hresults_are_recognised() {
        assert!(check("t", 0).is_ok());
        assert!(check("t", 1).is_ok(), "S_FALSE is a success code");
        let err = check("t", 0x8007_0005u32 as i32).unwrap_err();
        assert!(err.to_string().contains("80070005"), "{err}");
    }
}
