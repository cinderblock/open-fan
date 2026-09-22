//! Deciding what a takeover has to do, without doing any of it.
//!
//! Every judgement in a takeover lives here as a pure function over a described
//! situation: which channels are foreign-controlled, which applications are running, and
//! what is still alive after we asked them to stop. Execution elsewhere is then a thin
//! loop that cannot make a decision of its own.
//!
//! That split is not tidiness. The guard refusing to hand channels back while a rival
//! controller is still alive previously lived inline in a command-line example, where
//! `cargo test` never runs assertions — it was silently lost in an edit, shipped missing,
//! and a run did exactly what it forbade. Decisions that matter belong somewhere they can
//! be tested.

use of_hal::{ChannelControl, ChannelId};

use crate::{Role, Running};

/// A channel as the hardware currently describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSituation {
    pub id: ChannelId,
    pub label: String,
    pub control: ChannelControl,
}

/// Everything a takeover decision is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Situation {
    pub channels: Vec<ChannelSituation>,
    pub apps: Vec<Running>,
}

/// Why a takeover cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// A rival controller is still running. Handing channels back to firmware now would
    /// start a fight: we set the mode, it sets manual again, and the user watches two
    /// programs argue over their fans.
    ControllerStillRunning { names: Vec<String> },
}

impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControllerStillRunning { names } => write!(
                f,
                "{} is still running. Handing these channels back now would put them \
                 straight into manual again and the two programs would fight over them.",
                names.join(", ")
            ),
        }
    }
}

/// What a takeover should do about the situation it found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Rival controllers that must stand down before we can drive anything.
    pub stop: Vec<Running>,
    /// Applications that can stay: they read the bus but do not drive fans.
    pub coexist: Vec<Running>,
    /// Channels under foreign manual control, which need returning to firmware or taking
    /// over by us. Left alone, nothing responds to temperature on them.
    pub strand: Vec<ChannelId>,
    /// Channels the firmware is already handling. Nothing to do.
    pub firmware: Vec<ChannelId>,
    /// Channels we already hold.
    pub ours: Vec<ChannelId>,
    /// Channels whose control we could not determine.
    pub unknown: Vec<ChannelId>,
}

impl Plan {
    /// Whether anything needs doing at all.
    pub fn is_noop(&self) -> bool {
        self.stop.is_empty() && self.strand.is_empty()
    }
}

/// Work out what the situation calls for.
pub fn plan(situation: &Situation) -> Plan {
    let mut plan = Plan::default();

    for channel in &situation.channels {
        let id = channel.id.clone();
        match channel.control {
            ChannelControl::Firmware => plan.firmware.push(id),
            ChannelControl::Ours => plan.ours.push(id),
            ChannelControl::Foreign => plan.strand.push(id),
            _ => plan.unknown.push(id),
        }
    }

    for app in &situation.apps {
        match app.app.role {
            // Only a controller has to go. Asking someone to close a monitor that shares
            // the bus perfectly well is noise, and noise costs trust in later warnings.
            Role::Controller => plan.stop.push(app.clone()),
            Role::Monitor | Role::Vendor => plan.coexist.push(app.clone()),
        }
    }

    plan
}

/// Whether channels may be handed back to firmware, given who is still alive.
///
/// Pure over the list the caller observed, so the guard is testable without processes.
/// The caller is responsible for that list being *fresh* — asking before stopping
/// anything will obviously block.
pub fn hand_back_blocked_by(alive: &[Running]) -> Option<Blocker> {
    let names: Vec<String> = alive
        .iter()
        .filter(|r| r.app.role == Role::Controller)
        .map(|r| format!("{} (pid {})", r.app.name, r.pid))
        .collect();

    if names.is_empty() {
        None
    } else {
        Some(Blocker::ControllerStillRunning { names })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known::lookup;

    fn running(process: &str, pid: u32) -> Running {
        Running {
            app: lookup(process).expect("known app"),
            pid,
            process_name: process.to_owned(),
        }
    }

    fn channel(id: &str, control: ChannelControl) -> ChannelSituation {
        ChannelSituation {
            id: id.to_owned(),
            label: id.to_owned(),
            control,
        }
    }

    #[test]
    fn a_live_controller_blocks_handing_channels_back() {
        // The guard that was lost in an edit and shipped missing. It is a test now.
        let blocker = hand_back_blocked_by(&[running("FanControl", 1234)])
            .expect("a live controller must block");

        match &blocker {
            Blocker::ControllerStillRunning { names } => {
                assert_eq!(names.len(), 1);
                assert!(names[0].contains("FanControl"), "{names:?}");
                assert!(names[0].contains("1234"), "{names:?}");
            }
        }
        // The message has to name who, or the user cannot act on it.
        assert!(blocker.to_string().contains("FanControl"));
    }

    #[test]
    fn a_monitor_does_not_block_anything() {
        // HWiNFO shares the bus correctly and drives no fans. Blocking on it would mean
        // demanding people close a program that was never the problem.
        assert_eq!(hand_back_blocked_by(&[running("HWiNFO64", 22)]), None);
    }

    #[test]
    fn vendor_software_does_not_block_either() {
        // Armoury Crate cannot be serialised against and will not be closed by us. Making
        // it a blocker would make takeover permanently impossible on ASUS boards.
        assert_eq!(
            hand_back_blocked_by(&[running("ArmourySocketServer", 9)]),
            None
        );
    }

    #[test]
    fn nothing_running_blocks_nothing() {
        assert_eq!(hand_back_blocked_by(&[]), None);
    }

    #[test]
    fn every_live_controller_is_named_not_just_the_first() {
        // Two rival controllers means two things to close; reporting one would send the
        // user round the loop twice.
        let blocker =
            hand_back_blocked_by(&[running("FanControl", 1), running("ArgusMonitor", 2)]).unwrap();
        let Blocker::ControllerStillRunning { names } = blocker;
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn channels_are_sorted_by_who_is_driving_them() {
        let situation = Situation {
            channels: vec![
                channel("pwm/0", ChannelControl::Firmware),
                channel("pwm/1", ChannelControl::Foreign),
                channel("pwm/2", ChannelControl::Ours),
                channel("pwm/3", ChannelControl::Unknown),
            ],
            apps: vec![],
        };

        let plan = plan(&situation);
        assert_eq!(plan.firmware, ["pwm/0"]);
        assert_eq!(plan.strand, ["pwm/1"]);
        assert_eq!(plan.ours, ["pwm/2"]);
        // Unknown must not be quietly folded into "fine" — it means no information.
        assert_eq!(plan.unknown, ["pwm/3"]);
    }

    #[test]
    fn only_controllers_are_asked_to_stop() {
        let situation = Situation {
            channels: vec![],
            apps: vec![
                running("FanControl", 1),
                running("HWiNFO64", 2),
                running("ArmourySocketServer", 3),
            ],
        };

        let plan = plan(&situation);
        assert_eq!(plan.stop.len(), 1);
        assert_eq!(plan.stop[0].app.key, "fancontrol");
        assert_eq!(plan.coexist.len(), 2);
    }

    #[test]
    fn a_machine_with_nothing_in_the_way_needs_no_takeover() {
        let situation = Situation {
            channels: vec![channel("pwm/0", ChannelControl::Firmware)],
            apps: vec![running("HWiNFO64", 2)],
        };
        assert!(plan(&situation).is_noop());
    }

    #[test]
    fn a_stranded_channel_alone_is_enough_to_need_work() {
        // No rival process running, but a channel abandoned in manual by something that
        // has already exited. Nothing is responding to temperature on it, so this is not
        // a no-op even though there is nobody to ask to stop.
        let situation = Situation {
            channels: vec![channel("pwm/1", ChannelControl::Foreign)],
            apps: vec![],
        };
        assert!(!plan(&situation).is_noop());
    }
}
