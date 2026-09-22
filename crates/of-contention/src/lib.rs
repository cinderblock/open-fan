//! Detecting other fan-control software, and standing it down.
//!
//! Two applications driving the same PWM channel is a tug-of-war neither one reports.
//! Both write every tick, the fan obeys whichever wrote last, and each tool's UI shows
//! the duty it believes it commanded. OpenFan therefore has to notice the other party and
//! deal with it explicitly rather than quietly join the fight.
//!
//! # Ordering is a safety property
//!
//! Stopping another fan controller **leaves its channels in manual mode with nobody
//! driving them.** The duty is frozen wherever it was; if the machine then heats up,
//! nothing responds. So stopping the other application is never the last step:
//!
//! 1. detect what is running and which channels show foreign control;
//! 2. stop it;
//! 3. **immediately** either acquire the channels or hand them back to the firmware;
//! 4. verify that something is now genuinely in charge.
//!
//! This crate owns steps 1 and 2 only, and deliberately knows nothing about hardware —
//! it is process management, not chip access, and it must stay testable without either.
//! Step 3 is the caller's job and the caller must be ready before calling [`stop`].
//!
//! # What "graceful" can and cannot mean
//!
//! Less than you would hope. A fan controller that is minimised to the tray typically has
//! **no main window at all** — observed on the reference machine, where the competing
//! application had `MainWindowHandle = 0` and eleven hidden top-level windows with
//! obfuscated class names. There is nothing to send a polite close to, and no contract
//! saying a third-party application will exit when asked.
//!
//! So [`stop`] tries in escalating order and *reports which rung it had to reach*, rather
//! than pretending every exit is equal:
//!
//! | Rung | Mechanism | Does the app get to clean up? |
//! | --- | --- | --- |
//! | [`Politeness::Close`] | `WM_CLOSE` to each top-level window | yes, if it honours it |
//! | [`Politeness::Quit`] | `WM_QUIT` to each GUI thread | usually — the message loop unwinds |
//! | [`Politeness::Terminate`] | `TerminateProcess` | **no** |
//!
//! That distinction matters for more than tidiness: an application that exits cleanly may
//! hand its channels back to the firmware on the way out, and one that is terminated
//! certainly will not. The caller should re-read the hardware afterwards either way and
//! believe the chip, not the exit code.

#![cfg_attr(not(windows), allow(dead_code))]

use std::time::Duration;

mod known;
pub mod plan;
#[cfg(windows)]
mod windows_impl;

pub use known::{KNOWN_APPS, KnownApp, Role, lookup};
pub use plan::{Blocker, ChannelSituation, Plan, Situation, hand_back_blocked_by, plan};

/// Anything that can go wrong standing another application down.
#[derive(Debug, thiserror::Error)]
pub enum ContentionError {
    #[error("could not enumerate processes: {0}")]
    Enumerate(String),

    #[error("could not stop {name} (pid {pid}): {detail}")]
    Stop {
        name: String,
        pid: u32,
        detail: String,
    },

    #[error("contention handling is only implemented on Windows")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, ContentionError>;

/// A known fan-control application found running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub app: &'static KnownApp,
    pub pid: u32,
    /// The executable name as the OS reports it.
    pub process_name: String,
}

/// How hard we were willing to push, and how hard we had to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Politeness {
    /// Ask its windows to close. The only rung that is unambiguously the app's own choice.
    Close,
    /// End its message loops. It usually still runs its shutdown path.
    Quit,
    /// Kill it. No cleanup, no chance to restore anything it was controlling.
    Terminate,
}

/// What happened when we asked an application to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// It was not running by the time we looked.
    AlreadyGone,
    /// It exited, and this is the gentlest rung that achieved it.
    Stopped { via: Politeness },
    /// It is still running after everything we were allowed to try.
    StillRunning { tried: Politeness },
}

impl StopOutcome {
    /// Whether the process is gone, however it went.
    pub fn is_stopped(&self) -> bool {
        matches!(self, Self::AlreadyGone | Self::Stopped { .. })
    }

    /// Whether the application was given a real chance to restore what it controlled.
    ///
    /// A terminated process ran no shutdown code, so anything it was driving is still
    /// exactly as it left it — which is the case the caller must be ready for.
    pub fn had_chance_to_clean_up(&self) -> bool {
        match self {
            Self::AlreadyGone => false,
            Self::Stopped { via } => *via != Politeness::Terminate,
            Self::StillRunning { .. } => false,
        }
    }
}

/// Every known fan-control application currently running.
///
/// Process presence alone is *suggestive, not conclusive*. A tool can be running while
/// controlling nothing, and a channel can be under foreign control with no known process
/// to blame — a driver left loaded, an uninstalled tool's leftovers, or firmware itself.
/// The authoritative evidence is on the chip; this narrows down who to ask about it.
pub fn detect() -> Result<Vec<Running>> {
    #[cfg(windows)]
    {
        windows_impl::detect()
    }
    #[cfg(not(windows))]
    {
        Err(ContentionError::Unsupported)
    }
}

/// Ask an application to stop, escalating no further than `limit`.
///
/// **The caller must already be able to take over the channels this application was
/// driving.** On return they are in manual mode with nobody commanding them.
pub fn stop(running: &Running, limit: Politeness, timeout: Duration) -> Result<StopOutcome> {
    #[cfg(windows)]
    {
        windows_impl::stop(running, limit, timeout)
    }
    #[cfg(not(windows))]
    {
        let _ = (running, limit, timeout);
        Err(ContentionError::Unsupported)
    }
}

/// Whether a process id is still alive.
pub fn is_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        windows_impl::is_running(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminated_app_is_stopped_but_got_no_chance_to_clean_up() {
        // The distinction the caller's next move depends on: a killed fan controller
        // restored nothing, so its channels are still in manual exactly as it left them.
        let killed = StopOutcome::Stopped {
            via: Politeness::Terminate,
        };
        assert!(killed.is_stopped());
        assert!(!killed.had_chance_to_clean_up());

        let closed = StopOutcome::Stopped {
            via: Politeness::Close,
        };
        assert!(closed.is_stopped());
        assert!(closed.had_chance_to_clean_up());
    }

    #[test]
    fn an_app_that_would_not_stop_is_not_reported_as_stopped() {
        let stubborn = StopOutcome::StillRunning {
            tried: Politeness::Quit,
        };
        assert!(!stubborn.is_stopped());
        assert!(!stubborn.had_chance_to_clean_up());
    }

    #[test]
    fn politeness_escalates_in_the_documented_order() {
        // `stop` walks these in order and reports the gentlest that worked, so the
        // ordering is behaviour rather than cosmetics.
        assert!(Politeness::Close < Politeness::Quit);
        assert!(Politeness::Quit < Politeness::Terminate);
    }

    #[test]
    fn detection_is_total() {
        // Must never panic, whatever is or is not running.
        let _ = detect();
    }
}
