//! The boundary between the OpenFan service and its editor.
//!
//! Fan control runs in a Windows service so it can start at boot, survive logoff and hold
//! the one elevated handle to the hardware. The editor is therefore a client: unelevated,
//! disposable, and never in the control loop. This crate is the whole of what passes
//! between them — the message types in [`protocol`] and the named-pipe transport in
//! [`pipe`].
//!
//! Nothing here makes a control decision. It carries requests to something that does.

pub mod pipe;
pub mod protocol;

pub use pipe::{RpcError, request, service_reachable};
pub use protocol::{
    PIPE_NAME, PROTOCOL_VERSION, Request, Response, SERVICE_DISPLAY_NAME, SERVICE_NAME,
};
