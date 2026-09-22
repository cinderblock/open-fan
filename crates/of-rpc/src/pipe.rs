//! Named-pipe transport between the service and the editor.
//!
//! Deliberately blocking and thread-per-connection rather than async. The service's whole
//! reason to exist is a control loop that must not miss its deadline; adding an async
//! runtime whose worker pool could be occupied by a slow client is the exact coupling the
//! architecture avoids elsewhere. A handful of editor connections do not need more.
//!
//! Framing is newline-delimited JSON: one request per line, one response per line. It is
//! readable in a log, debuggable with any pipe client, and a protocol test asserts no
//! message can contain an embedded newline.
//!
//! # Who may connect, and why that is the interesting part
//!
//! The service runs as LocalSystem and holds the only elevated handle to the fans. The
//! editor runs **unelevated**, as the logged-in user — that is the point of the service,
//! and it means the pipe must be reachable without administrator rights.
//!
//! So the pipe's DACL grants:
//!
//! | Who | What | Why |
//! | --- | --- | --- |
//! | `SY` LocalSystem | full | the service itself |
//! | `BA` Administrators | full | diagnostics and management |
//! | `AU` Authenticated Users | read + write | the unelevated editor |
//!
//! **This is a real trust decision, not an oversight.** Any process running as a
//! logged-in user can open this pipe and command the fans. That is the same authority the
//! user already has over their own desktop, and it is the unavoidable price of an
//! unelevated editor — the alternative is prompting for administrator every time someone
//! wants to look at a fan curve. Two things bound it: the protocol has no request that
//! can stop fan control or disable a safety limit, and every request is validated by the
//! service exactly as if it came from disk.
//!
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` is set so a second server cannot quietly attach itself
//! to the same name and impersonate the service.

use std::io::{BufRead, BufReader, BufWriter, Write};

use crate::protocol::{Request, Response};

/// Anything that can go wrong carrying a message.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("the OpenFan service is not running")]
    NotRunning,

    #[error("transport failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not understand the other end: {0}")]
    Protocol(#[from] serde_json::Error),

    #[error("the connection closed before a reply arrived")]
    Closed,

    #[error("named-pipe transport is only implemented on Windows")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, RpcError>;

/// Send one request and wait for its reply.
///
/// A fresh connection per exchange. The editor polls a handful of times a second, the
/// cost is negligible, and it means a wedged or abandoned connection can never leave the
/// service holding state for a client that has gone away.
pub fn request(message: &Request) -> Result<Response> {
    #[cfg(windows)]
    {
        windows_impl::request(message)
    }
    #[cfg(not(windows))]
    {
        let _ = message;
        Err(RpcError::Unsupported)
    }
}

/// Whether the service is listening.
pub fn service_reachable() -> bool {
    matches!(request(&Request::Hello), Ok(Response::Hello { .. }))
}

/// Serve requests until the process ends.
///
/// `handle` is called on a connection thread, never on the control loop's thread, so a
/// slow client cannot delay a tick.
#[cfg(windows)]
pub fn serve<H>(handle: H) -> Result<()>
where
    H: Fn(Request) -> Response + Send + Sync + 'static,
{
    windows_impl::serve(handle)
}

/// Read a request, answer it, repeat until the client goes away.
///
/// Shared by the real server and by tests, so the framing is exercised without a pipe.
pub(crate) fn converse<R, W, H>(reader: R, writer: W, handle: &H) -> Result<()>
where
    R: std::io::Read,
    W: Write,
    H: Fn(Request) -> Response,
{
    let reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        // A request we cannot parse is answered, not dropped. A client that sent
        // nonsense deserves to be told so rather than watching the pipe close.
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle(request),
            Err(e) => Response::Error {
                message: format!("malformed request: {e}"),
            },
        };

        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    Ok(())
}

#[cfg(windows)]
mod windows_impl {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::FromRawHandle;

    use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::core::{HSTRING, PCWSTR};

    use super::{Result, RpcError, converse};
    use crate::protocol::{PIPE_NAME, Request, Response};

    /// LocalSystem and Administrators get everything; authenticated users get read and
    /// write so an unelevated editor can talk to us. See the module documentation — this
    /// is a deliberate trust boundary, not a default.
    const PIPE_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)";

    const BUFFER: u32 = 64 * 1024;

    /// A pipe instance being handed to its own connection thread.
    ///
    /// `HANDLE` is a raw pointer and so is not `Send`, but a named-pipe instance has no
    /// thread affinity — the kernel object is owned by the process, not the thread that
    /// created it. Exactly one thread owns each instance at a time: `serve` creates it and
    /// immediately gives it away, and never touches it again.
    struct SendHandle(HANDLE);

    // SAFETY: see above. The handle is moved, never shared, and has no thread affinity.
    unsafe impl Send for SendHandle {}

    /// A security descriptor parsed from SDDL, freed on drop.
    struct Descriptor(PSECURITY_DESCRIPTOR);

    impl Drop for Descriptor {
        fn drop(&mut self) {
            if !self.0.0.is_null() {
                // SAFETY: the pointer came from
                // ConvertStringSecurityDescriptorToSecurityDescriptorW, which documents
                // LocalFree as the way to release it, and is not used again.
                unsafe {
                    let _ = LocalFree(Some(windows::Win32::Foundation::HLOCAL(self.0.0)));
                }
            }
        }
    }

    fn descriptor() -> Result<Descriptor> {
        let mut raw = PSECURITY_DESCRIPTOR::default();
        let sddl = HSTRING::from(PIPE_SDDL);

        // SAFETY: `sddl` is a valid NUL-terminated wide string that outlives the call, and
        // `raw` is a valid out-pointer. On success it owns an allocation we free on drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )
        }
        .map_err(|e| RpcError::Io(std::io::Error::other(format!("pipe security: {e}"))))?;

        Ok(Descriptor(raw))
    }

    pub fn serve<H>(handle: H) -> Result<()>
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        let handle = std::sync::Arc::new(handle);
        let name = HSTRING::from(PIPE_NAME);
        let mut first = true;

        loop {
            let security = descriptor()?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
                lpSecurityDescriptor: security.0.0,
                bInheritHandle: false.into(),
            };

            // FIRST_PIPE_INSTANCE only on the first instance: it is what stops another
            // process claiming this name before us, and cannot be set on later ones.
            let mut flags = PIPE_ACCESS_DUPLEX;
            if first {
                flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
                first = false;
            }

            // SAFETY: `name` outlives the call; `attributes` points at a descriptor alive
            // for the duration of this iteration.
            let pipe = unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    flags,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    BUFFER,
                    BUFFER,
                    0,
                    Some(&attributes),
                )
            };

            if pipe.is_invalid() {
                return Err(RpcError::Io(std::io::Error::last_os_error()));
            }

            // SAFETY: a freshly created pipe instance; blocking until a client arrives.
            let connected = unsafe { ConnectNamedPipe(pipe, None) };
            if let Err(e) = connected {
                // ERROR_PIPE_CONNECTED means a client beat us to it, which is success.
                const ALREADY: i32 = 535;
                if e.code().0 & 0xFFFF != ALREADY {
                    // SAFETY: closing a handle we own.
                    unsafe {
                        let _ = CloseHandle(pipe);
                    }
                    continue;
                }
            }

            let handle = std::sync::Arc::clone(&handle);
            let owned = SendHandle(pipe);
            std::thread::Builder::new()
                .name("openfan-rpc".into())
                .spawn(move || {
                    let owned = owned;
                    serve_one(owned.0, handle.as_ref());
                })
                .map_err(RpcError::Io)?;
        }
    }

    /// Own one connection for its lifetime and hand the handle back to the OS at the end.
    fn serve_one<H>(pipe: HANDLE, handle: &H)
    where
        H: Fn(Request) -> Response,
    {
        // SAFETY: `pipe` is a live handle we own and do not use again; `File` takes
        // ownership and closes it on drop.
        let file = unsafe { std::fs::File::from_raw_handle(pipe.0 as _) };
        let writer = match file.try_clone() {
            Ok(writer) => writer,
            Err(e) => {
                tracing::warn!(error = %e, "could not split the pipe for writing");
                return;
            }
        };

        if let Err(e) = converse(file, writer, handle) {
            // A client that goes away mid-conversation is ordinary, not an incident.
            tracing::debug!(error = %e, "client connection ended");
        }

        // SAFETY: disconnecting a pipe instance we served; the handle is closed by the
        // `File` drop that follows.
        unsafe {
            let _ = DisconnectNamedPipe(pipe);
        }
    }

    pub fn request(message: &Request) -> Result<Response> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');

        let pipe = OpenOptions::new()
            .read(true)
            .write(true)
            .attributes(0)
            .open(PIPE_NAME)
            .map_err(|e| {
                // "not found" and "busy" both mean "not available to you right now", and
                // a user reading a log wants to know it is the service, not a bug here.
                let os = e.raw_os_error().unwrap_or(0);
                if os == 2 || os == ERROR_PIPE_BUSY.0 as i32 {
                    RpcError::NotRunning
                } else {
                    RpcError::Io(e)
                }
            })?;

        let mut writer = pipe.try_clone().map_err(RpcError::Io)?;
        writer.write_all(line.as_bytes())?;
        writer.flush()?;

        let mut reply = String::new();
        BufReader::new(pipe).read_line(&mut reply)?;
        if reply.trim().is_empty() {
            return Err(RpcError::Closed);
        }

        Ok(serde_json::from_str(&reply)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PROTOCOL_VERSION;

    /// The conversation loop over plain buffers, so framing is tested without a pipe.
    fn exchange(input: &str) -> String {
        let mut output = Vec::new();
        converse(input.as_bytes(), &mut output, &|request| match request {
            Request::Hello => Response::Hello {
                version: "test".into(),
                protocol: PROTOCOL_VERSION,
            },
            _ => Response::Ok,
        })
        .expect("converse");
        String::from_utf8(output).expect("utf8")
    }

    #[test]
    fn a_request_gets_exactly_one_line_back() {
        let out = exchange("{\"kind\":\"hello\"}\n");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("\"protocol\""), "{out}");
    }

    #[test]
    fn several_requests_on_one_connection_are_answered_in_order() {
        let out = exchange("{\"kind\":\"hello\"}\n{\"kind\":\"rescan\"}\n");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(lines[0].contains("hello"), "{out}");
        assert!(lines[1].contains("ok"), "{out}");
    }

    #[test]
    fn nonsense_is_answered_rather_than_dropped() {
        // A client that sends garbage must be told so. Closing the pipe instead leaves it
        // unable to tell a protocol bug from the service being gone.
        let out = exchange("not json at all\n");
        assert!(out.contains("malformed request"), "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
    }

    #[test]
    fn blank_lines_are_ignored_rather_than_answered() {
        let out = exchange("\n\n{\"kind\":\"rescan\"}\n");
        assert_eq!(out.lines().count(), 1, "{out}");
    }

    #[test]
    fn an_unreachable_service_is_named_as_such() {
        // On a machine with no service running this is the ordinary first-run state, and
        // the editor renders it as guidance rather than an error.
        match request(&Request::Hello) {
            Ok(_) => {}
            Err(RpcError::NotRunning) => {}
            Err(other) => panic!("unexpected transport failure: {other}"),
        }
    }
}
