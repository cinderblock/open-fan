//! Taking fan control over from another application.
//!
//! Orchestration only. Every judgement is made by [`of_contention::plan`] and
//! [`of_contention::hand_back_blocked_by`], which are pure and tested; every hardware
//! action goes through the engine, which owns the backend. This module's job is to call
//! them in the right order and turn the result into something a person can read.
//!
//! # The order, and why
//!
//! 1. Read the chip. Which channels the firmware is handling, which we hold, and which
//!    are in manual under nobody we know about.
//! 2. Name the applications running, split into those that must stand down and those
//!    that can stay.
//! 3. Stop the rivals, recording how gently each went.
//! 4. **Re-check who is alive**, and refuse to go further if a rival survived. Handing
//!    channels back underneath a live controller starts a fight the user then watches.
//! 5. Hand the stranded channels to the device's own curve.
//!
//! Step 5 is the one that means no reboot is needed: a firmware curve's configuration
//! lives in registers the mode selector does not touch, so putting the mode back restarts
//! the board's own curve with the board's own settings.
//!
//! # Why stopping comes with a warning
//!
//! Between steps 3 and 5 the rival's channels are in manual with **nobody driving them**.
//! The duty is frozen; if the machine heats up, nothing responds. That window is short
//! and the flow closes it deliberately, but it is why a takeover is a single operation
//! and not a button that only stops things.

use std::time::Duration;

use of_contention::{ChannelSituation, Politeness, Role, Situation, StopOutcome};
use of_hal::ChannelControl;
use of_ipc::{
    ChannelControlDto, ChannelControlEntry, ContendingAppDto, ContentionReport, TakeoverResult,
};

use crate::state::AppState;

/// How long to give a rival application to exit, across all politeness rungs.
const STOP_TIMEOUT: Duration = Duration::from_secs(12);

fn control_dto(control: ChannelControl) -> ChannelControlDto {
    match control {
        ChannelControl::Firmware => ChannelControlDto::Firmware,
        ChannelControl::Ours => ChannelControlDto::Ours,
        ChannelControl::Foreign => ChannelControlDto::Foreign,
        _ => ChannelControlDto::Unknown,
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Controller => "controller",
        Role::Monitor => "monitor",
        Role::Vendor => "vendor",
    }
}

/// Look at the machine and describe what stands between us and fan control.
pub(crate) fn survey(state: &AppState) -> ContentionReport {
    let labels: std::collections::BTreeMap<String, String> = state
        .engine
        .inventory()
        .channels
        .into_iter()
        .map(|c| (c.id, c.label))
        .collect();

    let controls = state.engine.channel_controls();
    let apps = of_contention::detect().unwrap_or_default();

    let situation = Situation {
        channels: controls
            .iter()
            .map(|(id, control)| ChannelSituation {
                id: id.clone(),
                label: labels.get(id).cloned().unwrap_or_else(|| id.clone()),
                control: *control,
            })
            .collect(),
        apps: apps.clone(),
    };
    let plan = of_contention::plan(&situation);

    ContentionReport {
        channels: situation
            .channels
            .iter()
            .map(|c| ChannelControlEntry {
                id: c.id.clone(),
                label: c.label.clone(),
                control: control_dto(c.control),
            })
            .collect(),
        apps: apps
            .iter()
            .map(|r| ContendingAppDto {
                key: r.app.key.to_owned(),
                name: r.app.name.to_owned(),
                process_name: r.process_name.clone(),
                pid: r.pid,
                role: role_name(r.app.role).to_owned(),
                note: r.app.note.to_owned(),
                must_stop: r.app.role == Role::Controller,
            })
            .collect(),
        stranded: plan.strand.clone(),
        clear: plan.is_noop(),
        blocker: None,
    }
}

/// What is in the way of OpenFan controlling this machine's fans.
#[tauri::command]
pub fn contention_report(state: tauri::State<'_, AppState>) -> ContentionReport {
    survey(&state)
}

/// Stand rival controllers down and hand abandoned channels back to the firmware.
///
/// `force` permits escalating to terminating a process that will not exit politely.
/// Off by default, and worth leaving off: a terminated application runs no shutdown code,
/// so it restores nothing it was controlling.
#[tauri::command]
pub fn take_over(state: tauri::State<'_, AppState>, force: bool) -> TakeoverResult {
    let mut steps = Vec::new();

    let before = survey(&state);
    if before.clear {
        steps.push("Nothing was in the way; no changes made.".to_owned());
        return TakeoverResult {
            steps,
            succeeded: true,
            blocker: None,
            report: before,
        };
    }

    // The takeover is the moment the user asks OpenFan to touch their fans, so it is
    // where control is switched on — not at startup. Until here the app has read
    // everything and written nothing.
    state.engine.enable_control();

    // --- stop the rivals -------------------------------------------------------------
    let limit = if force {
        Politeness::Terminate
    } else {
        Politeness::Quit
    };

    for app in of_contention::detect().unwrap_or_default() {
        if app.app.role != Role::Controller {
            continue;
        }

        match of_contention::stop(&app, limit, STOP_TIMEOUT) {
            Ok(StopOutcome::AlreadyGone) => {
                steps.push(format!("{} was not running.", app.app.name));
            }
            Ok(StopOutcome::Stopped { via }) => {
                steps.push(match via {
                    Politeness::Terminate => format!(
                        "{} would not exit and was terminated. It ran no shutdown code, so \
                         it restored nothing it was controlling.",
                        app.app.name
                    ),
                    _ => format!("{} exited ({via:?}).", app.app.name),
                });
            }
            Ok(StopOutcome::StillRunning { tried }) => {
                steps.push(format!(
                    "{} is still running after trying {tried:?}.",
                    app.app.name
                ));
            }
            Err(e) => steps.push(format!("Could not stop {}: {e}", app.app.name)),
        }
    }

    // --- refuse to continue under a live rival ---------------------------------------
    // Deliberately re-detected rather than reusing the list above: the question is who is
    // alive *now*, after everything we just did.
    let alive = of_contention::detect().unwrap_or_default();
    if let Some(blocker) = of_contention::hand_back_blocked_by(&alive) {
        let text = blocker.to_string();
        steps.push(format!("Stopped: {text}"));
        let mut report = survey(&state);
        report.blocker = Some(text.clone());
        return TakeoverResult {
            steps,
            succeeded: false,
            blocker: Some(text),
            report,
        };
    }

    // --- rescue the stranded channels -------------------------------------------------
    let stranded = survey(&state).stranded;
    if stranded.is_empty() {
        steps.push("Every channel is accounted for; nothing needed handing back.".to_owned());
    } else {
        for (id, outcome) in state.engine.hand_back_to_firmware(stranded) {
            steps.push(match outcome {
                Ok(()) => format!("{id} handed back to the board's own fan curve."),
                Err(e) => format!("{id} could not be handed back: {e}"),
            });
        }
    }

    let report = survey(&state);
    let succeeded = report.stranded.is_empty();
    if succeeded {
        steps.push(
            "Every channel is now either under the board firmware or under OpenFan.".to_owned(),
        );
    }

    TakeoverResult {
        steps,
        succeeded,
        blocker: None,
        report,
    }
}
