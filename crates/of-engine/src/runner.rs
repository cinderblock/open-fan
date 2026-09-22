//! The threaded control loop.
//!
//! This is the only module in the crate that touches wall-clock time or threads. It runs
//! the engine on its **own dedicated OS thread**, not on an async executor: the control
//! loop must not be able to miss its deadline because the UI, an updater download or a
//! filesystem call is occupying the runtime's worker pool.
//!
//! Commands reach the loop by channel and are drained between ticks, so an edit arriving
//! from the UI can never interleave with an evaluation in progress.
//!
//! # Shutdown
//!
//! The loop always runs [`Engine::shutdown`] on its way out — on a normal stop, and on a
//! panic inside the tick, because the shutdown lives in the same scope as the loop and a
//! panicking thread still unwinds. That is the last line of the dying breath that this
//! crate can guarantee on its own; covering a hard kill needs the external watchdog
//! (Phase 4).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use of_core::{Graph, GraphError};
use parking_lot::Mutex;

use crate::engine::{Engine, EngineConfig, Inventory, TickReport};
use crate::policy::SafetyPolicy;

/// A published view of the engine's most recent tick, for the UI.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub report: TickReport,
    /// Monotonic counter, so a consumer can tell a fresh tick from a repeated read.
    pub sequence: u64,
    /// Wall-clock milliseconds the last tick took to evaluate and apply.
    pub tick_duration_ms: f64,
    /// Ticks that overran their period. A rising count means the machine cannot keep up
    /// and the tick rate should come down.
    pub overruns: u64,
}

/// Work handed to the loop between ticks.
enum Command {
    SetGraph(Graph, Sender<Result<(), Vec<GraphError>>>),
    SetPolicy(SafetyPolicy),
    Rescan,
}

/// Handle to a running control loop.
///
/// Dropping the handle stops the loop and waits for the dying breath to complete.
pub struct EngineHandle {
    commands: Sender<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    /// The graph the engine is actually running. Published by the loop after a
    /// successful install, so a reader can never see an edit that was rejected.
    graph: Arc<Mutex<Graph>>,
    inventory: Arc<Mutex<Inventory>>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// Start the control loop on its own thread.
    pub fn spawn(engine: Engine) -> Self {
        let config = engine.config();
        let (tx, rx) = channel::<Command>();
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let graph = Arc::new(Mutex::new(engine.graph().clone()));
        let inventory = Arc::new(Mutex::new(engine.inventory()));
        let running = Arc::new(AtomicBool::new(true));

        let thread_snapshot = Arc::clone(&snapshot);
        let thread_graph = Arc::clone(&graph);
        let thread_inventory = Arc::clone(&inventory);
        let thread_running = Arc::clone(&running);

        let thread = thread::Builder::new()
            .name("openfan-control".into())
            .spawn(move || {
                // The engine is moved into a wrapper whose Drop hands the channels back.
                // Drop runs on a normal return *and* during a panic unwind, so a bug in
                // evaluation cannot leave the chip in manual mode with nobody driving it.
                let mut owner = ControlThread { engine };
                run_loop(
                    &mut owner.engine,
                    &rx,
                    &Published {
                        snapshot: thread_snapshot,
                        graph: thread_graph,
                        inventory: thread_inventory,
                    },
                    &thread_running,
                    config,
                );
            })
            .expect("spawn control thread");

        Self {
            commands: tx,
            snapshot,
            graph,
            inventory,
            running,
            thread: Some(thread),
        }
    }

    /// The graph the engine is running.
    pub fn graph(&self) -> Graph {
        self.graph.lock().clone()
    }

    /// What the backend can see.
    pub fn inventory(&self) -> Inventory {
        self.inventory.lock().clone()
    }

    /// Most recent published tick.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().clone()
    }

    /// Install a graph, waiting for the loop to validate and apply it.
    ///
    /// Returns the validation errors when the graph is rejected; the running
    /// configuration is untouched in that case.
    pub fn set_graph(&self, graph: Graph) -> Result<(), Vec<GraphError>> {
        let (tx, rx) = channel();
        if self.commands.send(Command::SetGraph(graph, tx)).is_err() {
            return Ok(());
        }
        rx.recv().unwrap_or(Ok(()))
    }

    pub fn set_policy(&self, policy: SafetyPolicy) {
        let _ = self.commands.send(Command::SetPolicy(policy));
    }

    pub fn rescan(&self) {
        let _ = self.commands.send(Command::Rescan);
    }

    /// Stop the loop and wait for the dying breath to finish.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Owns the engine for the lifetime of the control thread, and hands every channel back
/// when that thread leaves its scope — including during a panic unwind.
///
/// This is why the engine is moved into the thread rather than borrowed: ownership is
/// what makes the dying breath unconditional. `panic = "unwind"` is set in the release
/// profile specifically so this can run.
struct ControlThread {
    engine: Engine,
}

impl Drop for ControlThread {
    fn drop(&mut self) {
        let applied = self.engine.shutdown();
        tracing::info!(
            channels = applied.failsafed.len(),
            errors = applied.write_errors.len(),
            "control loop shut down; channels handed back"
        );
    }
}

/// The state the loop publishes for readers outside the control thread.
struct Published {
    snapshot: Arc<Mutex<Snapshot>>,
    graph: Arc<Mutex<Graph>>,
    inventory: Arc<Mutex<Inventory>>,
}

fn run_loop(
    engine: &mut Engine,
    commands: &Receiver<Command>,
    published: &Published,
    running: &Arc<AtomicBool>,
    config: EngineConfig,
) {
    let period = Duration::from_secs_f64(config.tick_period_seconds());
    let mut previous = Instant::now();
    let mut sequence = 0u64;
    let mut overruns = 0u64;

    while running.load(Ordering::SeqCst) {
        let cycle_start = Instant::now();

        // Drain pending edits before evaluating, so a tick always sees one coherent
        // configuration rather than one being changed underneath it.
        loop {
            match commands.try_recv() {
                Ok(Command::SetGraph(graph, reply)) => {
                    let outcome = engine.set_graph(graph);
                    // Publish only on success: a reader must never observe a graph the
                    // engine refused to run.
                    if outcome.is_ok() {
                        *published.graph.lock() = engine.graph().clone();
                    }
                    let _ = reply.send(outcome);
                }
                Ok(Command::SetPolicy(policy)) => engine.set_policy(policy),
                Ok(Command::Rescan) => {
                    engine.rescan();
                    *published.inventory.lock() = engine.inventory();
                }
                Err(TryRecvError::Empty) => break,
                // The handle is gone; the guard will hand the channels back.
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(previous).as_secs_f64();
        previous = now;

        let report = engine.tick(dt);

        let elapsed = cycle_start.elapsed();
        if elapsed > period {
            overruns += 1;
        }
        sequence += 1;

        *published.snapshot.lock() = Snapshot {
            report,
            sequence,
            tick_duration_ms: elapsed.as_secs_f64() * 1000.0,
            overruns,
        };

        // Sleep only the remainder, so a slow tick does not compound into drift.
        if let Some(rest) = period.checked_sub(elapsed) {
            thread::sleep(rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineConfig;
    use of_core::{CurvePoint, NodeKind, PortRef};
    use of_hal_mock::MockBackend;
    use of_units::Quantity;

    fn engine() -> Engine {
        Engine::new(
            Box::new(MockBackend::default()),
            EngineConfig {
                tick_hz: 200.0,
                ..Default::default()
            },
        )
    }

    fn good_graph() -> Graph {
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
                    CurvePoint { x: 30.0, y: 20.0 },
                    CurvePoint { x: 80.0, y: 100.0 },
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

    /// Wait for the loop to publish at least `n` ticks.
    fn wait_for_ticks(handle: &EngineHandle, n: u64) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snap = handle.snapshot();
            if snap.sequence >= n {
                return snap;
            }
            assert!(Instant::now() < deadline, "control loop did not tick");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_loop_ticks_and_publishes_snapshots() {
        let handle = EngineHandle::spawn(engine());
        handle.set_graph(good_graph()).unwrap();

        let snap = wait_for_ticks(&handle, 5);
        assert!(snap.sequence >= 5);
        assert!(
            snap.report
                .applied
                .commanded
                .contains_key(MockBackend::CHANNEL)
        );
        handle.stop();
    }

    #[test]
    fn a_rejected_graph_reports_its_errors_without_stopping_the_loop() {
        let handle = EngineHandle::spawn(engine());
        handle.set_graph(good_graph()).unwrap();
        wait_for_ticks(&handle, 2);

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

        let errors = handle.set_graph(bad).unwrap_err();
        assert!(!errors.is_empty());

        // Still running, still controlling.
        let before = handle.snapshot().sequence;
        let after = wait_for_ticks(&handle, before + 3);
        assert!(!after.report.degraded);
        handle.stop();
    }

    #[test]
    fn dropping_the_handle_stops_the_loop() {
        let handle = EngineHandle::spawn(engine());
        handle.set_graph(good_graph()).unwrap();
        wait_for_ticks(&handle, 3);

        let snapshot = Arc::clone(&handle.snapshot);
        drop(handle);

        // The loop has joined, so the sequence can no longer advance.
        let settled = snapshot.lock().sequence;
        thread::sleep(Duration::from_millis(50));
        assert_eq!(snapshot.lock().sequence, settled);
    }

    #[test]
    fn dt_is_measured_and_positive() {
        let handle = EngineHandle::spawn(engine());
        handle.set_graph(good_graph()).unwrap();
        let snap = wait_for_ticks(&handle, 5);
        assert!(
            snap.report.dt > 0.0 && snap.report.dt.is_finite(),
            "dt was {}",
            snap.report.dt
        );
        handle.stop();
    }
}
