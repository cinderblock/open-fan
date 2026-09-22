//! The command surface the editor is allowed to call.
//!
//! Every command is a thin translation between the engine and the transport DTOs in
//! `of-ipc`. **No control decisions happen here.** If a command looks like it is deciding
//! something about fans, it belongs in `of-engine` instead — the window can be closed or
//! crashed at any moment, and nothing about cooling may depend on it existing.
//!
//! Reads are cheap and lock-free enough to poll: the control loop publishes its state and
//! these commands clone it. A slow or wedged UI therefore cannot stall a tick.

use of_engine::EngineHandle;
use of_ipc::{
    ChannelDto, Graph, HardwareInventory, NodeDescriptor, PortTypeDto, SensorDto, SnapshotDto,
    ValidationError, WireValue, catalogue,
};

use crate::state::AppState;

/// Whether kernel-level hardware access is available on this machine.
///
/// Lives here rather than in the crate root because `#[tauri::command]` on a `pub fn`
/// at the root collides with the re-export the macro generates.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HardwareStatus {
    /// True when the driver OpenFan needs for sensor and PWM access is installed.
    pub driver_present: bool,
    /// Driver version, when it could be read.
    pub driver_version: Option<String>,
    /// A sentence the UI can show the user verbatim.
    pub summary: String,
}

#[tauri::command]
pub fn hardware_status() -> HardwareStatus {
    #[cfg(windows)]
    {
        match of_hal_pawnio::library_version() {
            Ok((major, minor, patch)) => HardwareStatus {
                driver_present: true,
                driver_version: Some(format!("{major}.{minor}.{patch}")),
                summary: "Hardware access is available.".into(),
            },
            Err(err) => HardwareStatus {
                driver_present: false,
                driver_version: None,
                // The error types carry install guidance; surface it rather than a code.
                summary: err.to_string(),
            },
        }
    }

    #[cfg(not(windows))]
    HardwareStatus {
        driver_present: false,
        driver_version: None,
        summary: "Hardware access on this platform is not implemented yet.".into(),
    }
}

/// Node kinds the editor can offer.
#[tauri::command]
pub fn node_catalogue() -> Vec<NodeDescriptor> {
    catalogue()
}

/// What the active backend can see.
#[tauri::command]
pub fn inventory(state: tauri::State<'_, AppState>) -> HardwareInventory {
    inventory_dto(&state.engine, hardware_status())
}

fn inventory_dto(engine: &EngineHandle, driver: HardwareStatus) -> HardwareInventory {
    let inv = engine.inventory();

    HardwareInventory {
        backend: inv.backend,
        sensors: inv
            .sensors
            .into_iter()
            .map(|s| SensorDto {
                id: s.id,
                label: s.label,
                quantity: of_engine_quantity(s.kind),
            })
            .collect(),
        channels: inv
            .channels
            .into_iter()
            .map(|c| ChannelDto {
                id: c.id,
                label: c.label,
                tachometer: c.tachometer,
                min_reliable_duty: c.min_reliable_duty,
            })
            .collect(),
        driver_present: driver.driver_present,
        driver_summary: driver.summary,
    }
}

/// The graph the engine is running.
#[tauri::command]
pub fn get_graph(state: tauri::State<'_, AppState>) -> Graph {
    state.engine.graph()
}

/// Install a graph.
///
/// On rejection the running configuration is untouched and every problem is returned at
/// once, so the editor can mark them all rather than making the user fix them one by one.
#[tauri::command]
pub fn set_graph(
    state: tauri::State<'_, AppState>,
    graph: Graph,
) -> Result<(), Vec<ValidationError>> {
    state.engine.set_graph(graph).map_err(|errors| {
        errors
            .into_iter()
            .map(|e| ValidationError {
                node_id: offending_node(&e),
                message: e.to_string(),
            })
            .collect()
    })
}

/// Infer the type of every port in a candidate graph.
///
/// Called by the editor on each edit so ports can be coloured as their types resolve.
/// Pure and cheap, and it does not require the graph to be valid — a half-built graph is
/// exactly when this is most useful.
#[tauri::command]
pub fn resolve_types(graph: Graph) -> Vec<PortTypeDto> {
    of_ipc::resolve_types(&graph)
}

/// Re-enumerate the backend's sensors and channels.
#[tauri::command]
pub fn rescan(state: tauri::State<'_, AppState>) {
    state.engine.rescan();
}

/// The engine's most recent tick.
#[tauri::command]
pub fn snapshot(state: tauri::State<'_, AppState>) -> SnapshotDto {
    snapshot_dto(&state.engine)
}

fn snapshot_dto(engine: &EngineHandle) -> SnapshotDto {
    let snap = engine.snapshot();
    let report = snap.report;

    SnapshotDto {
        sequence: snap.sequence,
        dt: report.dt,
        tick_duration_ms: snap.tick_duration_ms,
        overruns: snap.overruns,
        degraded: report.degraded,
        sensor_error: report.sensors.error,
        sensors: report
            .sensors
            .values
            .iter()
            .map(|(id, value)| WireValue::sensor(id, value))
            .collect(),
        wires: report
            .wire_values
            .iter()
            .map(|(port, value)| WireValue::new(port, value))
            .collect(),
        commanded: report.applied.commanded,
        failsafed: report.applied.failsafed.into_keys().collect(),
    }
}

/// Which node the editor should highlight for a validation error, when it names one.
fn offending_node(error: &of_core::GraphError) -> Option<String> {
    use of_core::GraphError as E;
    Some(match error {
        E::UnknownNode(id) | E::Cycle(id) | E::UnknownPort(id, _) => id.0.clone(),
        E::NotAnOutput { edge_from } => edge_from.node.0.clone(),
        E::NotAnInput { edge_to } => edge_to.node.0.clone(),
        E::TypeMismatch { to, .. } => to.node.0.clone(),
        // A conflict is attributed to the node that cannot satisfy both sides.
        E::TypeConflict { node, .. } => node.0.clone(),
        E::InputOverSubscribed(port) | E::MissingInput(port) => port.node.0.clone(),
    })
}

/// Translate a HAL sensor kind into the graph's vocabulary.
///
/// Mirrors the engine's own mapping. An unrecognised kind becomes `Ratio` here — this is
/// only a label in a picker, and a sensor the engine cannot type is one it also refuses
/// to read, so the user seeing it listed vaguely is the harmless end of that.
fn of_engine_quantity(kind: of_hal::SensorKind) -> of_ipc::Quantity {
    use of_hal::SensorKind as K;
    use of_ipc::Quantity as Q;
    match kind {
        K::Temperature => Q::Temperature,
        K::Fan => Q::Rpm,
        K::Voltage => Q::Voltage,
        K::Current => Q::Current,
        K::Power => Q::Power,
        K::Load => Q::Load,
        _ => Q::Ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use of_core::{NodeKind, PortRef};
    use of_units::Quantity;

    /// A live engine over the simulated plant, running the canonical curve graph.
    fn running_engine() -> (EngineHandle, String) {
        use of_core::CurvePoint;
        use of_hal_mock::MockBackend;

        let engine = of_engine::Engine::new(
            Box::new(MockBackend::default()),
            of_engine::EngineConfig {
                tick_hz: 200.0,
                ..Default::default()
            },
        );
        let handle = EngineHandle::spawn(engine);

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
                    CurvePoint { x: 0.0, y: 40.0 },
                    CurvePoint { x: 100.0, y: 100.0 },
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
        handle.set_graph(g).expect("graph should be valid");

        // Wait for a tick that actually evaluated the graph.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let snap = handle.snapshot();
            if snap.sequence > 0 && !snap.report.degraded {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "engine never ticked");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        (handle, MockBackend::CHANNEL.to_owned())
    }

    #[test]
    fn the_snapshot_carries_live_values_the_editor_can_index() {
        let (engine, channel) = running_engine();
        let dto = snapshot_dto(&engine);

        assert!(dto.sequence > 0);
        assert!(!dto.degraded);
        assert!(dto.dt > 0.0 && dto.dt.is_finite());
        assert!(dto.commanded.contains_key(&channel));

        // Wire values must be addressable by the ids the editor renders nodes with.
        let curve = dto
            .wires
            .iter()
            .find(|w| w.node_id == "curve" && w.port == "out")
            .expect("the curve output should appear on a wire");
        assert!(curve.value.is_some(), "a healthy tick must carry a number");
        assert_eq!(curve.quantity, Quantity::Duty);

        engine.stop();
    }

    #[test]
    fn a_degraded_tick_says_so_rather_than_reporting_stale_numbers() {
        let (engine, _) = running_engine();

        // Unload the profile: every channel is failsafed and nothing is commanded.
        engine.set_graph(Graph::default()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let dto = loop {
            let dto = snapshot_dto(&engine);
            if dto.commanded.is_empty() {
                break dto;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "engine kept commanding"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };

        assert!(dto.commanded.is_empty());
        assert!(
            dto.wires.is_empty(),
            "an empty graph has no wires to report"
        );

        engine.stop();
    }

    #[test]
    fn the_inventory_lists_what_the_backend_advertises() {
        let (engine, channel) = running_engine();
        let dto = inventory_dto(
            &engine,
            HardwareStatus {
                driver_present: false,
                driver_version: None,
                summary: "no driver".into(),
            },
        );

        assert_eq!(dto.backend, "Simulated thermal plant");
        assert!(dto.channels.iter().any(|c| c.id == channel));
        assert!(
            dto.sensors
                .iter()
                .any(|s| s.quantity == Quantity::Temperature),
            "the plant advertises a temperature: {:?}",
            dto.sensors
        );
        // Driver state is passed through verbatim so the UI can show it without
        // reinterpreting it.
        assert!(!dto.driver_present);
        assert_eq!(dto.driver_summary, "no driver");

        engine.stop();
    }

    #[test]
    fn a_rejected_graph_is_reported_per_node_and_changes_nothing() {
        let (engine, channel) = running_engine();
        let before = engine.graph();

        // Temperature straight into a duty input.
        let mut bad = Graph::default();
        bad.insert(
            "t",
            NodeKind::Sensor {
                sensor_id: "s".into(),
                quantity: Quantity::Temperature,
            },
        );
        bad.insert(
            "fan",
            NodeKind::FanOutput {
                channel: channel.clone(),
            },
        );
        bad.connect(PortRef::new("t", "out"), PortRef::new("fan", "duty"));

        let errors = engine.set_graph(bad).unwrap_err();
        assert!(!errors.is_empty());
        for e in &errors {
            assert!(
                offending_node(e).is_some(),
                "{e:?} has no node to highlight"
            );
        }
        assert_eq!(
            engine.graph(),
            before,
            "a rejected edit must change nothing"
        );

        engine.stop();
    }

    #[test]
    fn every_validation_error_names_a_node_to_highlight() {
        // An error the editor cannot attach to anything is an error the user has to hunt
        // for by hand, which defeats reporting them all at once.
        let mut g = Graph::default();
        g.insert(
            "t",
            NodeKind::Sensor {
                sensor_id: "s".into(),
                quantity: Quantity::Temperature,
            },
        );
        g.insert(
            "fan",
            NodeKind::FanOutput {
                channel: "c".into(),
            },
        );
        g.connect(PortRef::new("t", "out"), PortRef::new("fan", "duty"));

        let errors = g.validate().unwrap_err();
        assert!(!errors.is_empty());
        for e in &errors {
            assert!(
                offending_node(e).is_some(),
                "{e:?} cannot be attributed to a node"
            );
        }
    }
}
