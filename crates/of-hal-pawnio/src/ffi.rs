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

    #[error(
        "PawnIO is installed at {path}, but the PawnIOLib.dll there could not be loaded \
         ({detail}). The installation may be damaged; reinstalling from \
         https://pawnio.eu should fix it."
    )]
    LibraryUnusable { path: String, detail: String },

    #[error("PawnIOLib is missing the {0} export; the installed version may be too old")]
    MissingExport(&'static str),

    #[error(
        "PawnIO refused access (E_ACCESSDENIED). Its driver only accepts a handle from an \
         elevated process, so OpenFan must run as administrator to read sensors or control \
         fans at all."
    )]
    AccessDenied,

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

    #[error(
        "PawnIO hardware module {module:?} was not found. PawnIO installs the driver only; \
         the hardware modules are a separate download from \
         https://github.com/namazso/PawnIO.Modules/releases. Searched: {searched}"
    )]
    ModuleNotFound { module: String, searched: String },

    #[error("function name {0:?} is not valid for FFI")]
    InvalidName(String),
}

type Result<T> = std::result::Result<T, PawnIoError>;

/// `E_ACCESSDENIED`. PawnIO returns this for every call made from a process that is not
/// elevated, so it is worth recognising rather than printing as a hex code.
const E_ACCESSDENIED: Hresult = 0x8007_0005u32 as Hresult;

fn check(call: &'static str, hr: Hresult) -> Result<()> {
    match hr {
        hr if hr >= 0 => Ok(()),
        E_ACCESSDENIED => Err(PawnIoError::AccessDenied),
        hr => Err(PawnIoError::Call { call, hresult: hr }),
    }
}

/// Locating `PawnIOLib.dll`.
///
/// The installer puts the library in `C:\Program Files\PawnIO\` and adds that directory to
/// neither `PATH` nor `System32`, so a bare `LoadLibrary("PawnIOLib.dll")` fails on a
/// machine where PawnIO is installed, running and working. Reporting that as "not
/// installed" is worse than unhelpful: a backend trusting [`is_available`] would silently
/// fall back to the mock on a machine with real fans. So we look the directory up.
mod resolve {
    use std::path::{Path, PathBuf};

    pub const LIB_NAME: &str = "PawnIOLib.dll";

    /// Turn a service `ImagePath` into the directory holding the driver.
    ///
    /// Values look like `\??\C:\Program Files\PawnIO\PawnIO.sys`, but the NT prefix is
    /// optional, `\SystemRoot\` is also used, and a path may be relative to the Windows
    /// directory. Pure, so it is tested without touching a registry.
    pub fn image_path_to_dir(image_path: &str, system_root: &str) -> Option<PathBuf> {
        let trimmed = image_path.trim();

        let rebased = if let Some(rest) = trimmed.strip_prefix(r"\??\") {
            rest.to_owned()
        } else if let Some(rest) = trimmed.strip_prefix(r"\SystemRoot\") {
            format!(r"{system_root}\{rest}")
        } else {
            trimmed.to_owned()
        };

        if rebased.is_empty() {
            return None;
        }

        let path = PathBuf::from(&rebased);
        let absolute = if path.is_absolute() {
            path
        } else {
            Path::new(system_root).join(path)
        };

        absolute
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
    }

    /// Where the *currently loaded* driver lives — the best source available, because it
    /// describes the driver Windows actually has rather than one that merely left
    /// registry keys behind.
    fn dir_from_service() -> Option<PathBuf> {
        let key = windows_registry::LOCAL_MACHINE
            .open(r"SYSTEM\CurrentControlSet\Services\PawnIO")
            .ok()?;
        let image_path = key.get_string("ImagePath").ok()?;
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
        image_path_to_dir(&image_path, &system_root)
    }

    /// `InstallLocation` from the uninstall entry, which covers a PawnIO that is installed
    /// but whose service is not currently registered.
    fn dir_from_uninstall_key() -> Option<PathBuf> {
        let uninstall = windows_registry::LOCAL_MACHINE
            .open(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall")
            .ok()?;

        uninstall.keys().ok()?.find_map(|name| {
            let entry = uninstall.open(&name).ok()?;
            if entry.get_string("DisplayName").ok()? != "PawnIO" {
                return None;
            }
            let location = entry.get_string("InstallLocation").ok()?;
            let location = location.trim();
            (!location.is_empty()).then(|| PathBuf::from(location))
        })
    }

    /// Directories PawnIO may be installed in, best first, deduplicated.
    pub fn install_dirs() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();

        for dir in [
            dir_from_service(),
            dir_from_uninstall_key(),
            std::env::var_os("ProgramFiles").map(|pf| PathBuf::from(pf).join("PawnIO")),
        ]
        .into_iter()
        .flatten()
        {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }

        dirs
    }

    /// Full paths to try for the library, best first.
    ///
    /// The bare file name comes first so a copy placed beside our executable, or a
    /// directory the user has put on `PATH`, still wins — that is the escape hatch for a
    /// non-standard install.
    pub fn library_candidates() -> Vec<PathBuf> {
        let mut candidates = vec![PathBuf::from(LIB_NAME)];
        candidates.extend(install_dirs().into_iter().map(|dir| dir.join(LIB_NAME)));
        candidates
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
        let mut last_located_failure: Option<(std::path::PathBuf, String)> = None;

        // SAFETY: loading a library runs its initializers. PawnIOLib is a well-behaved
        // installed component; there is no safer alternative to LoadLibrary here.
        let library = resolve::library_candidates()
            .into_iter()
            .find_map(
                |candidate| match unsafe { libloading::Library::new(&candidate) } {
                    Ok(library) => Some(library),
                    Err(e) => {
                        // Only remember a failure at a path we had positive reason to believe
                        // in. The bare-name probe failing is the ordinary case on a correctly
                        // installed machine, and says nothing.
                        if candidate
                            .parent()
                            .is_some_and(|p| !p.as_os_str().is_empty())
                            && candidate.exists()
                        {
                            last_located_failure = Some((candidate, e.to_string()));
                        }
                        None
                    }
                },
            )
            .ok_or_else(|| match last_located_failure {
                // A library we found and still could not load means PawnIO is present but
                // broken, which needs a different fix from "go install it".
                Some((path, detail)) => PawnIoError::LibraryUnusable {
                    path: path.display().to_string(),
                    detail,
                },
                None => PawnIoError::NotInstalled,
            })?;

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

/// The per-user directory holding downloaded module blobs.
///
/// PawnIO's modules are LGPL-2.1 and not ours to redistribute, so they are fetched from
/// the upstream release rather than bundled. They land here: a user-writable location
/// needing no administrator rights and surviving a reinstall of either PawnIO or OpenFan.
///
/// Deliberately the *local* data directory, not the roaming one. These are signed
/// binaries matched to the hardware in this machine; syncing them onto a different
/// machine via a roaming profile is at best pointless and at worst confusing.
pub fn module_cache_dir() -> Option<std::path::PathBuf> {
    directories::ProjectDirs::from("", "", "OpenFan")
        .map(|dirs| dirs.data_local_dir().join("pawnio-modules"))
}

/// Directories searched for signed module blobs, best first.
///
/// Ours come before the PawnIO installation so a module version we have actually tested
/// wins over whatever else may have been dropped into a shared directory by another
/// application or an installer.
pub fn module_search_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        dirs.push(exe_dir.join("modules"));
        dirs.push(exe_dir.to_path_buf());
    }

    dirs.extend(module_cache_dir());

    for install in resolve::install_dirs() {
        dirs.push(install.join("modules"));
        dirs.push(install);
    }

    dirs
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
        let blob = std::fs::read(path).map_err(|_| PawnIoError::ModuleNotFound {
            module: path.display().to_string(),
            searched: path.display().to_string(),
        })?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::load_module(name, &blob)
    }

    /// Find an official signed module by name (e.g. `LpcIO`) and load it.
    ///
    /// Modules are **not** ours to redistribute, so they are read from disk at runtime
    /// rather than embedded. Note that PawnIO's own installer ships *no* modules — they
    /// are a separate download — so "module missing" is a routine first-run state that
    /// deserves its own explanation, not a driver-missing error and not a crash.
    pub fn load_module_by_name(module_name: &str) -> Result<Self> {
        let file_name = format!("{module_name}.bin");

        let searched: Vec<std::path::PathBuf> = module_search_dirs()
            .into_iter()
            .map(|dir| dir.join(&file_name))
            .collect();

        match searched.iter().find(|path| path.is_file()) {
            Some(path) => Self::load_module_from_path(path),
            None => Err(PawnIoError::ModuleNotFound {
                module: module_name.to_owned(),
                searched: searched
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }
}

impl std::fmt::Debug for PawnIo {
    // Hand-written rather than derived: the interesting state is which module is loaded,
    // not a page of function-pointer addresses from `Lib`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PawnIo")
            .field("module", &self.module)
            .field("open", &!self.handle.is_null())
            .finish()
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
    fn nt_prefixed_service_image_paths_resolve_to_the_install_dir() {
        // The exact value observed on the reference machine.
        let dir =
            resolve::image_path_to_dir(r"\??\C:\Program Files\PawnIO\PawnIO.sys", r"C:\Windows");
        assert_eq!(
            dir,
            Some(std::path::PathBuf::from(r"C:\Program Files\PawnIO"))
        );
    }

    #[test]
    fn system_root_and_relative_image_paths_are_rebased_on_the_windows_dir() {
        // Both spellings the SCM accepts for a driver living under the Windows directory.
        let expected = Some(std::path::PathBuf::from(r"C:\Windows\System32\drivers"));
        assert_eq!(
            resolve::image_path_to_dir(r"\SystemRoot\System32\drivers\x.sys", r"C:\Windows"),
            expected
        );
        assert_eq!(
            resolve::image_path_to_dir(r"System32\drivers\x.sys", r"C:\Windows"),
            expected
        );
    }

    #[test]
    fn junk_image_paths_do_not_produce_a_directory() {
        assert_eq!(resolve::image_path_to_dir("", r"C:\Windows"), None);
        assert_eq!(resolve::image_path_to_dir("   ", r"C:\Windows"), None);
    }

    #[test]
    fn the_bare_library_name_is_always_tried_first() {
        // Keeps the escape hatch working: a DLL beside our exe, or on PATH, must win over
        // whatever the registry claims is installed.
        let candidates = resolve::library_candidates();
        assert_eq!(candidates[0], std::path::PathBuf::from(resolve::LIB_NAME));
        assert!(
            candidates
                .iter()
                .all(|c| c.file_name() == Some(resolve::LIB_NAME.as_ref())),
            "every candidate must name the library: {candidates:?}"
        );
    }

    #[test]
    fn a_missing_module_explains_that_modules_ship_separately() {
        // The first-run state on a machine with PawnIO installed: driver present, no
        // module blobs. It must not be reported as a missing driver.
        let err = PawnIo::load_module_by_name("DefinitelyNotARealModule").unwrap_err();
        assert!(
            matches!(err, PawnIoError::ModuleNotFound { .. }),
            "unexpected error kind: {err}"
        );
        let text = err.to_string();
        assert!(text.contains("PawnIO.Modules"), "{text}");
    }

    #[test]
    fn failing_hresults_are_recognised() {
        assert!(check("t", 0).is_ok());
        assert!(check("t", 1).is_ok(), "S_FALSE is a success code");

        // An unrecognised failure still surfaces its code, so it can be looked up.
        let err = check("t", 0x8007_001Fu32 as i32).unwrap_err();
        assert!(err.to_string().contains("8007001F"), "{err}");
    }

    #[test]
    fn access_denied_is_reported_as_needing_elevation() {
        // The very first thing a non-elevated run hits. "HRESULT 0x80070005" reads as a
        // bug in us; "must run as administrator" is the actual, actionable cause.
        let err = check("pawnio_open", E_ACCESSDENIED).unwrap_err();
        assert!(matches!(err, PawnIoError::AccessDenied), "{err}");
        assert!(err.to_string().contains("administrator"), "{err}");
    }
}
