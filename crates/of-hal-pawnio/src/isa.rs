//! Arbitration for the ISA/LPC bus.
//!
//! Super I/O access is a two-step transaction: write a register number to an index port,
//! then read or write the data port next to it. The chip has exactly one index register,
//! so the sequence is not atomic and is not reentrant. If another process writes its own
//! index between our index write and our data read, **we read the wrong register** — and
//! symmetrically, we corrupt its transaction.
//!
//! That is not hypothetical on the reference machine, which had two other hardware tools
//! actively polling the same chip. A wrong read here is worse than a failed read: a
//! plausible-looking temperature from the wrong register is exactly the input that makes
//! a fan curve do the wrong thing confidently.
//!
//! Windows hardware monitoring tools coordinate through a long-established named mutex,
//! `Global\Access_ISABUS.HTP.Method`, and PawnIO's `LpcIO` module documents every one of
//! its ioctls as requiring it. Holding it is how we are a good citizen on a shared bus
//! rather than a source of corruption for everyone else.
//!
//! # What this does not cover
//!
//! ASUS's own `AsIO` driver holds a *kernel* mutex that user mode cannot acquire, so this
//! lock does not serialise against ASUS software touching the EC ports (`0x25C`/`0x25D`).
//! That contention has to be handled by not fighting over those ports at all.

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

/// Failure to arbitrate for the bus.
#[derive(Debug, thiserror::Error)]
pub enum IsaBusError {
    #[error("could not open the ISA bus mutex: {0}")]
    Unavailable(String),

    #[error(
        "timed out after {0:?} waiting for the ISA bus mutex. Another hardware monitoring \
         or fan control application is holding it and not letting go."
    )]
    Timeout(Duration),
}

/// An acquired hold on the ISA bus. Released on drop.
///
/// Deliberately `!Send`/`!Sync` by construction — a Windows mutex is owned by the thread
/// that waited on it and can only be released by that thread, so letting this cross a
/// thread boundary would produce a release that silently fails.
#[derive(Debug)]
pub struct IsaBusLock {
    handle: HANDLE,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl IsaBusLock {
    /// The name every Windows hardware monitoring tool has agreed on for decades.
    const NAME: windows::core::PCWSTR = w!("Global\\Access_ISABUS.HTP.Method");

    /// Wait for exclusive use of the bus.
    ///
    /// `CreateMutexW` opens the existing mutex when one is already there, so whichever
    /// process starts first creates it and the rest join.
    pub fn acquire(timeout: Duration) -> Result<Self, IsaBusError> {
        // SAFETY: a null security descriptor and a static name are both valid; we do not
        // request initial ownership, so the handle is usable regardless of who else holds
        // the mutex.
        let handle = unsafe { CreateMutexW(None, false, Self::NAME) }
            .map_err(|e| IsaBusError::Unavailable(e.to_string()))?;

        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: `handle` is a live mutex handle from the call above.
        let wait = unsafe { WaitForSingleObject(handle, millis) };

        match wait {
            WAIT_OBJECT_0 => Ok(Self {
                handle,
                _not_send: std::marker::PhantomData,
            }),

            // The previous owner died holding it. We now own it, and the bus may have
            // been left mid-transaction — but refusing to proceed would mean a crashed
            // third-party tool permanently locks us out of controlling the fans, which is
            // strictly worse. Take it; the caller re-establishes chip state anyway.
            WAIT_ABANDONED => Ok(Self {
                handle,
                _not_send: std::marker::PhantomData,
            }),

            _ => {
                // SAFETY: closing a handle we own and will not use again.
                let _ = unsafe { CloseHandle(handle) };
                Err(IsaBusError::Timeout(timeout))
            }
        }
    }
}

impl Drop for IsaBusLock {
    fn drop(&mut self) {
        // SAFETY: we hold this mutex on this thread, and the handle is live until the
        // CloseHandle below. Both failures are unrecoverable and unactionable here.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}
