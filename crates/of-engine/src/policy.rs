//! Failsafe policy, and the rule that turns an evaluated tick into hardware writes.
//!
//! This module is the core of safety layers 1 (per-channel floor), 4 (dying breath) and
//! the downgrade rule that keeps layer 4 honest on hardware that cannot hand control
//! back to firmware. See `plans/open-fan.md`.

use std::collections::BTreeMap;

use of_core::{Commands, TickResult};
use of_hal::{ChannelId, OutputChannel};

/// What to do with a channel when we cannot command it confidently.
///
/// The default is [`FailsafeAction::RestoreFirmware`]: hand the header back to the
/// BIOS/EC, which has its own fan curve and will keep running whatever happens to us.
/// Full duty is the fallback for hardware that cannot be handed back — loud, but alive.
///
/// Note what is *not* an option: holding the last commanded value. A stale duty from
/// before a sensor died looks like control and is not.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FailsafeAction {
    /// Restore the configuration captured when the channel was acquired. The default:
    /// the firmware's own curve is the most reliable thing left once we are gone.
    #[default]
    RestoreFirmware,
    /// Drive a fixed duty. Used when the backend cannot restore firmware control.
    FixedDuty(f64),
}

/// Per-channel safety configuration.
///
/// The derived default — restore firmware control, no floor — is the right behaviour for
/// a channel nobody has configured yet: we hand it straight back rather than pretending
/// to manage it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChannelPolicy {
    /// What to do when this channel faults, or when we are shutting down.
    pub failsafe: FailsafeAction,
    /// Duty this channel may never be driven below while under normal control.
    /// Safety layer 1.
    pub floor_duty: f64,
}

/// The resolved safety policy for every channel we control.
#[derive(Debug, Clone, Default)]
pub struct SafetyPolicy {
    channels: BTreeMap<ChannelId, ChannelPolicy>,
    /// Applied to a channel with no explicit entry. Unknown channels must still fail
    /// safe, so this cannot be "do nothing".
    default: ChannelPolicy,
}

impl SafetyPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_channel(mut self, channel: impl Into<ChannelId>, policy: ChannelPolicy) -> Self {
        self.channels.insert(channel.into(), policy);
        self
    }

    pub fn for_channel(&self, channel: &ChannelId) -> &ChannelPolicy {
        self.channels.get(channel).unwrap_or(&self.default)
    }

    /// Guarantee this channel has a policy entry, without disturbing an existing one.
    ///
    /// The engine calls this the moment it acquires a channel. `channels()` is what
    /// `apply_tick` iterates to decide who gets failsafed, so a channel we hold but have
    /// no entry for would be skipped every tick — held at a stale duty with nobody
    /// responsible for it. That is the failure this method exists to make impossible.
    pub fn ensure_channel(&mut self, channel: impl Into<ChannelId>) {
        self.channels.entry(channel.into()).or_default();
    }

    /// Every channel with an explicit policy.
    pub fn channels(&self) -> impl Iterator<Item = &ChannelId> {
        self.channels.keys()
    }
}

/// Outcome of applying one tick to hardware.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Applied {
    /// Channels driven from the graph, and the duty actually written.
    pub commanded: BTreeMap<ChannelId, f64>,
    /// Channels put into their failsafe this tick.
    pub failsafed: BTreeMap<ChannelId, FailsafeAction>,
    /// Channels whose hardware write returned an error.
    pub write_errors: Vec<ChannelId>,
}

/// Write one evaluated tick to hardware, enforcing the safety policy.
///
/// Every channel the policy knows about is accounted for on every tick: it is either
/// commanded from the graph or explicitly failsafed. A channel silently going unwritten
/// is the bug class this function exists to make impossible.
pub fn apply_tick(
    tick: &TickResult,
    policy: &SafetyPolicy,
    backend: &mut dyn OutputChannel,
) -> Applied {
    let mut applied = Applied::default();
    let can_restore = backend.can_restore_firmware_control();

    // Channels the graph produced a trustworthy value for, minus any that also faulted.
    for (channel, value) in commands_of(tick) {
        if tick.faulted_channels.contains(channel) {
            continue;
        }
        let floor = policy.for_channel(channel).floor_duty;
        let duty = value.scalar.max(floor).clamp(0.0, 100.0);
        match backend.set_duty(channel, duty) {
            Ok(()) => {
                applied.commanded.insert(channel.clone(), duty);
            }
            Err(_) => applied.write_errors.push(channel.clone()),
        }
    }

    // Everything else the policy covers gets its failsafe: channels the graph faulted,
    // and channels the graph does not mention at all.
    let needs_failsafe = policy
        .channels()
        .filter(|c| !applied.commanded.contains_key(*c))
        .cloned()
        .collect::<Vec<_>>();

    for channel in needs_failsafe
        .into_iter()
        .chain(tick.faulted_channels.iter().cloned())
    {
        if applied.failsafed.contains_key(&channel) {
            continue;
        }
        let action = resolve(policy.for_channel(&channel).failsafe, can_restore);
        let outcome = match action {
            FailsafeAction::RestoreFirmware => backend.release(&channel),
            FailsafeAction::FixedDuty(duty) => backend.set_duty(&channel, duty),
        };
        match outcome {
            Ok(()) => {
                applied.failsafed.insert(channel, action);
            }
            Err(_) => applied.write_errors.push(channel),
        }
    }

    applied
}

/// Put every channel into its failsafe and let go of the hardware.
///
/// This is the dying breath. It runs from panic handlers, from the Windows shutdown
/// handler and on normal exit, so it must stay simple: no allocation beyond the small
/// result map, no locks, no async, no logging through a subscriber that may already be
/// torn down. Errors are counted, never propagated — there is nobody left to handle them.
pub fn dying_breath(policy: &SafetyPolicy, backend: &mut dyn OutputChannel) -> Applied {
    let mut applied = Applied::default();
    let can_restore = backend.can_restore_firmware_control();

    for channel in policy.channels().cloned().collect::<Vec<_>>() {
        let action = resolve(policy.for_channel(&channel).failsafe, can_restore);
        let outcome = match action {
            FailsafeAction::RestoreFirmware => backend.release(&channel),
            FailsafeAction::FixedDuty(duty) => backend.set_duty(&channel, duty),
        };
        match outcome {
            Ok(()) => {
                applied.failsafed.insert(channel, action);
            }
            // A failed restore is the worst case: the chip may still be in manual mode.
            // Try full duty before giving up on this channel.
            Err(_) => {
                if backend.set_duty(&channel, 100.0).is_ok() {
                    applied
                        .failsafed
                        .insert(channel, FailsafeAction::FixedDuty(100.0));
                } else {
                    applied.write_errors.push(channel);
                }
            }
        }
    }

    applied
}

/// Downgrade `RestoreFirmware` to full duty on backends that cannot actually restore.
/// Releasing such a channel would leave the chip in manual mode with nobody driving it.
fn resolve(action: FailsafeAction, can_restore: bool) -> FailsafeAction {
    match action {
        FailsafeAction::RestoreFirmware if !can_restore => FailsafeAction::FixedDuty(100.0),
        other => other,
    }
}

fn commands_of(tick: &TickResult) -> &Commands {
    &tick.commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use of_core::{EvalState, Graph, MixMode, NodeKind, PortRef, SensorReadings};
    use of_hal::OutputChannel;
    use of_hal_mock::MockBackend;
    use of_units::{Quantity, Value};

    /// The engine's default 10 Hz cadence.
    const DT: f64 = 0.1;

    fn policy() -> SafetyPolicy {
        SafetyPolicy::new().with_channel(
            MockBackend::CHANNEL,
            ChannelPolicy {
                failsafe: FailsafeAction::RestoreFirmware,
                floor_duty: 20.0,
            },
        )
    }

    fn graph() -> Graph {
        let mut g = Graph::default();
        g.insert(
            "t",
            NodeKind::Sensor {
                sensor_id: MockBackend::TEMP_SENSOR.into(),
                quantity: Quantity::Temperature,
            },
        );
        g.insert(
            "curve",
            NodeKind::Curve {
                points: vec![
                    of_core::node::CurvePoint { x: 30.0, y: 0.0 },
                    of_core::node::CurvePoint { x: 80.0, y: 100.0 },
                ],
            },
        );
        g.insert(
            "fan",
            NodeKind::FanOutput {
                channel: MockBackend::CHANNEL.into(),
            },
        );
        g.connect(PortRef::new("t", "out"), PortRef::new("curve", "in"));
        g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));
        g
    }

    #[test]
    fn a_healthy_tick_drives_the_channel() {
        let compiled = graph().validate().unwrap();
        let mut backend = MockBackend::default();
        backend.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();

        let readings = SensorReadings::from([(
            MockBackend::TEMP_SENSOR.to_owned(),
            Value::raw(Quantity::Temperature, 55.0),
        )]);
        let tick = compiled.tick_with(&readings, DT, &mut EvalState::new());
        let applied = apply_tick(&tick, &policy(), &mut backend);

        assert_eq!(applied.commanded[MockBackend::CHANNEL], 50.0);
        assert!(applied.failsafed.is_empty());
    }

    #[test]
    fn the_floor_duty_cannot_be_undercut_by_the_graph() {
        let compiled = graph().validate().unwrap();
        let mut backend = MockBackend::default();
        backend.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();

        // 25 °C is below the curve's first point, so the graph asks for 0 %.
        let readings = SensorReadings::from([(
            MockBackend::TEMP_SENSOR.to_owned(),
            Value::raw(Quantity::Temperature, 25.0),
        )]);
        let tick = compiled.tick_with(&readings, DT, &mut EvalState::new());
        let applied = apply_tick(&tick, &policy(), &mut backend);

        assert_eq!(
            applied.commanded[MockBackend::CHANNEL],
            20.0,
            "floor must win over the graph"
        );
    }

    #[test]
    fn a_faulted_channel_is_failsafed_not_left_at_its_last_value() {
        let compiled = graph().validate().unwrap();
        let mut backend = MockBackend::default();
        backend.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();
        let policy = policy();

        // Healthy tick first, so there is a "last value" to wrongly hold.
        let readings = SensorReadings::from([(
            MockBackend::TEMP_SENSOR.to_owned(),
            Value::raw(Quantity::Temperature, 55.0),
        )]);
        apply_tick(
            &compiled.tick_with(&readings, DT, &mut EvalState::new()),
            &policy,
            &mut backend,
        );

        // Now the sensor disappears.
        let tick = compiled.tick_with(&SensorReadings::new(), DT, &mut EvalState::new());
        let applied = apply_tick(&tick, &policy, &mut backend);

        assert!(applied.commanded.is_empty());
        assert_eq!(
            applied.failsafed[MockBackend::CHANNEL],
            FailsafeAction::RestoreFirmware
        );
        assert!(backend.released, "the header must actually be handed back");
    }

    #[test]
    fn a_channel_the_graph_never_mentions_is_still_failsafed() {
        // The bug class: a user deletes the fan node but the channel stays acquired.
        let empty = Graph::default().validate().unwrap();
        let mut backend = MockBackend::default();
        backend.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();

        let applied = apply_tick(
            &empty.tick_with(&SensorReadings::new(), DT, &mut EvalState::new()),
            &policy(),
            &mut backend,
        );
        assert!(applied.failsafed.contains_key(MockBackend::CHANNEL));
    }

    #[test]
    fn restore_downgrades_to_full_duty_when_the_backend_cannot_restore() {
        struct NoRestore(MockBackend);
        impl OutputChannel for NoRestore {
            fn channels(&self) -> of_hal::Result<Vec<of_hal::ChannelInfo>> {
                self.0.channels()
            }
            fn acquire(&mut self, c: &ChannelId) -> of_hal::Result<()> {
                self.0.acquire(c)
            }
            fn set_duty(&mut self, c: &ChannelId, p: f64) -> of_hal::Result<()> {
                self.0.set_duty(c, p)
            }
            fn release(&mut self, c: &ChannelId) -> of_hal::Result<()> {
                self.0.release(c)
            }
            fn can_restore_firmware_control(&self) -> bool {
                false
            }
        }

        let mut backend = NoRestore(MockBackend::default());
        backend.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();
        let applied = dying_breath(&policy(), &mut backend);

        assert_eq!(
            applied.failsafed[MockBackend::CHANNEL],
            FailsafeAction::FixedDuty(100.0)
        );
        assert!(
            !backend.0.released,
            "must not release a header it cannot hand back"
        );
    }

    #[test]
    fn the_dying_breath_covers_every_policy_channel() {
        let mut backend = MockBackend::default();
        backend.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();

        let applied = dying_breath(&policy(), &mut backend);
        assert_eq!(applied.failsafed.len(), 1);
        assert!(applied.write_errors.is_empty());
        assert!(backend.released);
    }

    #[test]
    fn mixing_in_a_dead_sensor_failsafes_rather_than_cooling_less() {
        let mut g = Graph::default();
        g.insert(
            "a",
            NodeKind::Sensor {
                sensor_id: "a".into(),
                quantity: Quantity::Temperature,
            },
        );
        g.insert(
            "b",
            NodeKind::Sensor {
                sensor_id: "b".into(),
                quantity: Quantity::Temperature,
            },
        );
        g.insert("mix", NodeKind::Mix { mode: MixMode::Max });
        g.insert(
            "curve",
            NodeKind::Curve {
                points: vec![
                    of_core::node::CurvePoint { x: 30.0, y: 0.0 },
                    of_core::node::CurvePoint { x: 80.0, y: 100.0 },
                ],
            },
        );
        g.insert(
            "fan",
            NodeKind::FanOutput {
                channel: MockBackend::CHANNEL.into(),
            },
        );
        g.connect(PortRef::new("a", "out"), PortRef::new("mix", "in"));
        g.connect(PortRef::new("b", "out"), PortRef::new("mix", "in"));
        g.connect(PortRef::new("mix", "out"), PortRef::new("curve", "in"));
        g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));

        let compiled = g.validate().unwrap();
        let mut backend = MockBackend::default();
        backend.acquire(&MockBackend::CHANNEL.to_owned()).unwrap();

        let readings =
            SensorReadings::from([("a".to_owned(), Value::raw(Quantity::Temperature, 95.0))]);
        let applied = apply_tick(
            &compiled.tick_with(&readings, DT, &mut EvalState::new()),
            &policy(),
            &mut backend,
        );

        assert!(applied.commanded.is_empty());
        assert!(applied.failsafed.contains_key(MockBackend::CHANNEL));
    }
}
