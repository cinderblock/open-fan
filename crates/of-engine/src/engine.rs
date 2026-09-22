//! The engine: owns the hardware backend and the active graph, and performs one tick
//! when told to.
//!
//! It reads no clock. `dt` is an argument, which is what makes every behaviour here —
//! including recovery from a sensor dropping out mid-run, or a backend that stops
//! responding — reproducible in a test with no timing and no hardware. [`crate::runner`]
//! is the only place wall-clock time enters.
//!
//! # The invariant this module exists to hold
//!
//! **Every acquired channel is written on every tick — commanded or failsafed.** There is
//! no path through [`Engine::tick`] that leaves a channel untouched. A channel that is
//! merely *not mentioned* by the current graph is still ours until we release it, and
//! leaving it at whatever duty it happened to have is how a machine ends up silently
//! uncooled.

use std::collections::{BTreeMap, BTreeSet};

use of_core::{CompiledGraph, EvalState, Graph, GraphError, PortRef, SensorReadings};
use of_hal::{Backend, ChannelControl, ChannelId, ChannelInfo, SensorId, SensorInfo, SensorKind};
use of_units::{Quantity, Value};

use crate::policy::{Applied, SafetyPolicy, apply_tick, dying_breath};

/// Engine tuning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineConfig {
    /// Nominal evaluation rate.
    pub tick_hz: f64,
    /// Upper bound on `dt`, in seconds.
    ///
    /// A laptop resuming from sleep, or a machine that was thrashing, can hand the engine
    /// an enormous elapsed time. Integrating that would step every filter and every PID
    /// integrator straight to saturation, so it is clamped: the loop treats a long gap as
    /// a single long-ish tick and lets the filters re-converge, rather than pretending it
    /// observed the whole interval.
    pub max_dt_seconds: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            tick_hz: 10.0,
            max_dt_seconds: 1.0,
        }
    }
}

impl EngineConfig {
    pub fn tick_period_seconds(&self) -> f64 {
        1.0 / self.tick_hz.max(0.1)
    }
}

/// What the sensors reported this tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SensorSnapshot {
    pub values: BTreeMap<SensorId, Value>,
    /// Set when the backend could not be read at all.
    pub error: Option<String>,
}

/// The outcome of one tick.
#[derive(Debug, Clone, Default)]
pub struct TickReport {
    pub applied: Applied,
    /// Every value on every wire, for the editor's live readouts.
    pub wire_values: BTreeMap<PortRef, Value>,
    pub sensors: SensorSnapshot,
    pub faulted_channels: BTreeSet<ChannelId>,
    /// The `dt` actually used, after clamping.
    pub dt: f64,
    /// True when no graph was evaluated — no profile loaded, or the sensors could not be
    /// read. Every acquired channel was failsafed.
    pub degraded: bool,
}

/// What the active backend can see on this machine.
///
/// Captured at construction and refreshed by [`Engine::rescan`]. The editor renders this
/// to offer sensors and channels, so it is a view rather than part of the document: a
/// profile referencing a sensor that is no longer present must still load, and fail
/// loudly at the sensor instead of refusing to open.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub backend: String,
    pub sensors: Vec<SensorInfo>,
    pub channels: Vec<ChannelInfo>,
}

/// Owns the backend and the active graph.
pub struct Engine {
    backend: Box<dyn Backend>,
    config: EngineConfig,
    /// Quantity for each sensor the backend enumerated, so a raw `f64` can be tagged
    /// with what it actually measures before it reaches the graph.
    sensor_quantities: BTreeMap<SensorId, Quantity>,
    /// Which sensor reads back each channel. Handed to the graph each tick so a fan can
    /// report its own measured speed without the document storing hardware wiring.
    tachometers: BTreeMap<ChannelId, SensorId>,
    graph: Graph,
    compiled: Option<CompiledGraph>,
    eval: EvalState,
    policy: SafetyPolicy,
    acquired: BTreeSet<ChannelId>,
}

impl Engine {
    /// Create an engine over a backend, enumerating its sensors.
    pub fn new(backend: Box<dyn Backend>, config: EngineConfig) -> Self {
        let sensor_quantities: BTreeMap<SensorId, Quantity> = backend
            .sensors()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| quantity_of(s.kind).map(|q| (s.id, q)))
            .collect();

        let tachometers = tachometers_of(backend.as_ref());

        Self {
            backend,
            config,
            sensor_quantities,
            tachometers,
            graph: Graph::default(),
            compiled: None,
            eval: EvalState::new(),
            policy: SafetyPolicy::new(),
            acquired: BTreeSet::new(),
        }
    }

    pub fn backend_name(&self) -> String {
        self.backend.name()
    }

    /// Everything the backend advertises, for the editor's pickers.
    pub fn inventory(&self) -> Inventory {
        Inventory {
            backend: self.backend.name(),
            sensors: self.backend.sensors().unwrap_or_default(),
            channels: self.backend.channels().unwrap_or_default(),
        }
    }

    pub fn config(&self) -> EngineConfig {
        self.config
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn policy(&self) -> &SafetyPolicy {
        &self.policy
    }

    pub fn set_policy(&mut self, policy: SafetyPolicy) {
        self.policy = policy;
        // Re-assert coverage: a replacement policy must not drop a channel we hold.
        for channel in &self.acquired {
            self.policy.ensure_channel(channel.clone());
        }
    }

    /// Sensors the backend advertises.
    pub fn available_sensors(&self) -> &BTreeMap<SensorId, Quantity> {
        &self.sensor_quantities
    }

    /// Re-enumerate the backend's sensors and channels.
    pub fn rescan(&mut self) {
        self.sensor_quantities = self
            .backend
            .sensors()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| quantity_of(s.kind).map(|q| (s.id, q)))
            .collect();
        self.tachometers = tachometers_of(self.backend.as_ref());
    }

    /// Install a new graph.
    ///
    /// Validates first and changes nothing on failure, so a bad edit cannot take the
    /// running configuration down with it. Channels the new graph no longer drives are
    /// released back to firmware rather than left held at their last duty.
    pub fn set_graph(&mut self, graph: Graph) -> Result<(), Vec<GraphError>> {
        let compiled = graph.validate()?;

        let wanted = graph.output_channels();
        let to_release: Vec<ChannelId> = self
            .acquired
            .iter()
            .filter(|c| !wanted.contains(*c))
            .cloned()
            .collect();

        for channel in to_release {
            // Best effort: a header we can no longer drive is still better off back under
            // firmware control than held at a duty nothing is updating.
            let _ = self.backend.release(&channel);
            self.acquired.remove(&channel);
        }

        for channel in wanted {
            if self.acquired.contains(&channel) {
                continue;
            }
            match self.backend.acquire(&channel) {
                Ok(()) => {
                    self.acquired.insert(channel.clone());
                    // Coverage is established at acquire time, so apply_tick can never
                    // meet a channel it holds but has no policy for.
                    self.policy.ensure_channel(channel);
                }
                Err(err) => {
                    tracing::warn!(%channel, %err, "could not acquire channel");
                }
            }
        }

        self.eval.retain_nodes(&graph);
        self.graph = graph;
        self.compiled = Some(compiled);
        Ok(())
    }

    /// Evaluate and apply one tick.
    pub fn tick(&mut self, dt: f64) -> TickReport {
        let dt = sanitize_dt(dt, &self.config);

        let raw = match self.backend.read_all() {
            Ok(values) => values,
            Err(err) => {
                // Blind. Nothing about the previous tick's numbers is evidence about
                // now, so do not evaluate the graph at all — failsafe and say so.
                let message = err.to_string();
                tracing::error!(error = %message, "sensor read failed; failsafing");
                return self.degrade(
                    dt,
                    SensorSnapshot {
                        values: BTreeMap::new(),
                        error: Some(message),
                    },
                );
            }
        };

        // Tag each raw reading with what the backend says it measures. A sensor the
        // backend did not enumerate is dropped rather than guessed at.
        let sensors: SensorReadings = raw
            .into_iter()
            .filter_map(|(id, scalar)| {
                // `Value::new` saturates into the quantity's physical limits, so a
                // glitched register cannot reach the graph as a duty of 4000 %.
                self.sensor_quantities
                    .get(&id)
                    .map(|q| (id, Value::new(*q, scalar)))
            })
            .collect();

        let snapshot = SensorSnapshot {
            values: sensors.clone(),
            error: None,
        };

        let Some(compiled) = self.compiled.as_ref() else {
            // No profile loaded. Anything we hold goes back to firmware.
            return self.degrade(dt, snapshot);
        };

        let tick = compiled.tick(
            &of_core::TickInput::new(&sensors, dt).with_tachometers(&self.tachometers),
            &mut self.eval,
        );
        let applied = apply_tick(&tick, &self.policy, self.backend.as_mut());

        TickReport {
            applied,
            wire_values: tick.wire_values,
            sensors: snapshot,
            faulted_channels: tick.faulted_channels,
            dt,
            degraded: false,
        }
    }

    /// Failsafe every acquired channel and report it as a degraded tick.
    fn degrade(&mut self, dt: f64, sensors: SensorSnapshot) -> TickReport {
        let applied = dying_breath(&self.policy, self.backend.as_mut());
        TickReport {
            faulted_channels: self.acquired.clone(),
            applied,
            wire_values: BTreeMap::new(),
            sensors,
            dt,
            degraded: true,
        }
    }

    /// Hand every channel back and stop controlling anything.
    ///
    /// Safe to call more than once.
    pub fn shutdown(&mut self) -> Applied {
        let applied = dying_breath(&self.policy, self.backend.as_mut());
        self.acquired.clear();
        self.compiled = None;
        applied
    }

    /// Channels currently under our control.
    /// Who is driving each of the backend's channels right now.
    ///
    /// Used by the takeover flow to tell a channel the firmware is handling from one
    /// another application abandoned in manual mode. A backend that cannot tell reports
    /// [`ChannelControl::Unknown`], which means *no information* — never "nobody else".
    pub fn channel_controls(&self) -> Vec<(ChannelId, ChannelControl)> {
        self.backend
            .channels()
            .unwrap_or_default()
            .into_iter()
            .map(|info| {
                let control = self
                    .backend
                    .control_of(&info.id)
                    .unwrap_or(ChannelControl::Unknown);
                (info.id, control)
            })
            .collect()
    }

    /// Hand channels to the device's own control algorithm.
    ///
    /// Recovery for channels some other application left in manual and abandoned, where
    /// nothing is responding to temperature. Refuses any channel **we** hold, because
    /// that is not abandoned — releasing one of ours is [`Engine::shutdown`]'s job and
    /// goes through the recorded state, not a mode we picked.
    ///
    /// Reports per channel rather than failing the batch: one header that will not take
    /// the write must not prevent the rest from being rescued.
    pub fn hand_back_to_firmware(
        &mut self,
        channels: &[ChannelId],
    ) -> Vec<(ChannelId, Result<(), String>)> {
        channels
            .iter()
            .map(|id| {
                let outcome = if self.acquired.contains(id) {
                    Err(format!(
                        "{id} is held by this engine; release it rather than overwriting                          its mode"
                    ))
                } else {
                    self.backend
                        .hand_back_to_firmware(id)
                        .map_err(|e| e.to_string())
                };
                (id.clone(), outcome)
            })
            .collect()
    }

    pub fn acquired_channels(&self) -> &BTreeSet<ChannelId> {
        &self.acquired
    }
}

/// Clamp `dt` into something an integrator can safely consume.
fn sanitize_dt(dt: f64, config: &EngineConfig) -> f64 {
    if !dt.is_finite() || dt <= 0.0 {
        // A zero or nonsense interval would divide by zero in the PID derivative.
        config.tick_period_seconds()
    } else {
        dt.min(config.max_dt_seconds)
    }
}

/// Which sensor reads back each channel, from what the backend advertises.
fn tachometers_of(backend: &dyn Backend) -> BTreeMap<ChannelId, SensorId> {
    backend
        .channels()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| c.tachometer.map(|t| (c.id, t)))
        .collect()
}

/// What each sensor kind measures, in the graph's vocabulary.
///
/// `None` for a kind this build does not recognise — `SensorKind` is `non_exhaustive`,
/// so a newer backend can report something we have no vocabulary for. Such a sensor is
/// dropped rather than guessed at: a reading typed as the wrong quantity is worse than
/// no reading, because the graph would happily steer on it.
fn quantity_of(kind: SensorKind) -> Option<Quantity> {
    Some(match kind {
        SensorKind::Temperature => Quantity::Temperature,
        SensorKind::Fan => Quantity::Rpm,
        SensorKind::Voltage => Quantity::Voltage,
        SensorKind::Current => Quantity::Current,
        SensorKind::Power => Quantity::Power,
        SensorKind::Load => Quantity::Load,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ChannelPolicy, FailsafeAction};
    use of_core::{NodeKind, PortRef};
    use of_hal::{ChannelInfo, HalError, OutputChannel, SensorInfo, SensorSource};
    use of_hal_mock::MockBackend;

    fn graph_for(channel: &str) -> Graph {
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
                    of_core::CurvePoint { x: 30.0, y: 0.0 },
                    of_core::CurvePoint { x: 80.0, y: 100.0 },
                ],
            },
        );
        g.insert(
            "fan",
            NodeKind::FanOutput {
                channel: channel.into(),
            },
        );
        g.connect(PortRef::new("t", "out"), PortRef::new("curve", "in"));
        g.connect(PortRef::new("curve", "out"), PortRef::new("fan", "duty"));
        g
    }

    fn engine() -> Engine {
        Engine::new(Box::new(MockBackend::default()), EngineConfig::default())
    }

    #[test]
    fn installing_a_graph_acquires_its_channels_and_covers_them_with_policy() {
        let mut e = engine();
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();

        assert!(e.acquired_channels().contains(MockBackend::CHANNEL));
        // The invariant: nothing we hold is outside the policy, so nothing we hold can
        // be skipped by apply_tick.
        assert!(e.policy().channels().any(|c| c == MockBackend::CHANNEL));
    }

    #[test]
    fn an_invalid_graph_is_rejected_without_disturbing_the_running_one() {
        let mut e = engine();
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();
        let good = e.graph().clone();

        // Temperature straight into a duty input.
        let mut bad = Graph::default();
        bad.insert(
            "t",
            NodeKind::Sensor {
                sensor_id: MockBackend::TEMP_SENSOR.into(),
                quantity: Quantity::Temperature,
            },
        );
        bad.insert(
            "fan",
            NodeKind::FanOutput {
                channel: MockBackend::CHANNEL.into(),
            },
        );
        bad.connect(PortRef::new("t", "out"), PortRef::new("fan", "duty"));

        assert!(e.set_graph(bad).is_err());
        assert_eq!(e.graph(), &good, "a rejected edit must change nothing");

        // And the engine is still controlling.
        let report = e.tick(0.1);
        assert!(!report.degraded);
    }

    #[test]
    fn a_tick_drives_the_channel_from_the_curve() {
        let mut e = engine();
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();

        // Plant starts at ambient (22 C), below the curve, so duty is 0 but still
        // commanded rather than skipped.
        let report = e.tick(0.1);
        assert!(!report.degraded);
        assert!(report.applied.commanded.contains_key(MockBackend::CHANNEL));
        assert!(report.applied.failsafed.is_empty());
        assert!(!report.wire_values.is_empty());
    }

    #[test]
    fn with_no_graph_every_acquired_channel_is_still_failsafed() {
        let mut e = engine();
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();
        e.compiled = None; // simulate a profile being unloaded

        let report = e.tick(0.1);
        assert!(report.degraded);
        assert!(report.applied.failsafed.contains_key(MockBackend::CHANNEL));
    }

    #[test]
    fn dropping_a_fan_node_releases_its_channel() {
        let mut e = engine();
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();
        assert!(!e.acquired_channels().is_empty());

        // A graph with no outputs at all.
        e.set_graph(Graph::default()).unwrap();
        assert!(
            e.acquired_channels().is_empty(),
            "a channel with no node responsible for it must be handed back"
        );
    }

    #[test]
    fn a_backend_that_stops_reading_failsafes_instead_of_reusing_stale_numbers() {
        /// A backend whose sensors fail after the first read.
        struct Flaky {
            inner: MockBackend,
            reads: usize,
        }

        impl SensorSource for Flaky {
            fn sensors(&self) -> of_hal::Result<Vec<SensorInfo>> {
                self.inner.sensors()
            }
            fn read_all(&mut self) -> of_hal::Result<BTreeMap<SensorId, f64>> {
                self.reads += 1;
                if self.reads > 1 {
                    Err(HalError::Io("bus wedged".into()))
                } else {
                    self.inner.read_all()
                }
            }
        }

        impl OutputChannel for Flaky {
            fn channels(&self) -> of_hal::Result<Vec<ChannelInfo>> {
                self.inner.channels()
            }
            fn acquire(&mut self, c: &ChannelId) -> of_hal::Result<()> {
                self.inner.acquire(c)
            }
            fn set_duty(&mut self, c: &ChannelId, p: f64) -> of_hal::Result<()> {
                self.inner.set_duty(c, p)
            }
            fn release(&mut self, c: &ChannelId) -> of_hal::Result<()> {
                self.inner.release(c)
            }
            fn can_restore_firmware_control(&self) -> bool {
                true
            }
        }

        impl Backend for Flaky {
            fn name(&self) -> String {
                "flaky".into()
            }
        }

        let mut e = Engine::new(
            Box::new(Flaky {
                inner: MockBackend::default(),
                reads: 0,
            }),
            EngineConfig::default(),
        );
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();

        assert!(!e.tick(0.1).degraded);

        let report = e.tick(0.1);
        assert!(report.degraded);
        assert_eq!(
            report.sensors.error.as_deref(),
            Some("hardware I/O failed: bus wedged")
        );
        assert!(report.applied.commanded.is_empty());
        assert!(report.applied.failsafed.contains_key(MockBackend::CHANNEL));
    }

    #[test]
    fn a_wildly_long_gap_is_clamped_rather_than_integrated() {
        let config = EngineConfig::default();
        // Resume-from-sleep: hours of wall clock, which would saturate every integrator.
        assert_eq!(sanitize_dt(7200.0, &config), config.max_dt_seconds);
        // Nonsense values fall back to the nominal period instead of dividing by zero.
        assert_eq!(sanitize_dt(0.0, &config), config.tick_period_seconds());
        assert_eq!(sanitize_dt(f64::NAN, &config), config.tick_period_seconds());
        assert_eq!(sanitize_dt(-1.0, &config), config.tick_period_seconds());
        assert_eq!(sanitize_dt(0.05, &config), 0.05);
    }

    #[test]
    fn a_sensor_reporting_the_wrong_quantity_faults_rather_than_steering_on_it() {
        let mut e = engine();
        // Ask for the fan tachometer but declare it a temperature.
        let mut g = Graph::default();
        g.insert(
            "t",
            NodeKind::Sensor {
                sensor_id: MockBackend::RPM_SENSOR.into(),
                quantity: Quantity::Temperature,
            },
        );
        g.insert(
            "curve",
            NodeKind::Curve {
                points: vec![
                    of_core::CurvePoint { x: 30.0, y: 0.0 },
                    of_core::CurvePoint { x: 80.0, y: 100.0 },
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

        e.set_graph(g).unwrap();
        let report = e.tick(0.1);

        assert!(report.applied.commanded.is_empty());
        assert!(report.faulted_channels.contains(MockBackend::CHANNEL));
    }

    #[test]
    fn shutdown_hands_everything_back() {
        let mut e = engine();
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();
        e.tick(0.1);

        let applied = e.shutdown();
        assert_eq!(
            applied.failsafed.get(MockBackend::CHANNEL),
            Some(&FailsafeAction::RestoreFirmware)
        );
        assert!(e.acquired_channels().is_empty());

        // Idempotent: a second call must not panic or error.
        let _ = e.shutdown();
    }

    #[test]
    fn replacing_the_policy_cannot_drop_a_held_channel() {
        let mut e = engine();
        e.set_graph(graph_for(MockBackend::CHANNEL)).unwrap();

        // A policy that knows nothing about the channel we hold.
        e.set_policy(
            SafetyPolicy::new().with_channel("some/other/channel", ChannelPolicy::default()),
        );

        assert!(
            e.policy().channels().any(|c| c == MockBackend::CHANNEL),
            "coverage must be re-asserted when the policy is replaced"
        );
    }
}
