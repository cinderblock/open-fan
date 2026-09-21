# OpenFan — Living Plan

> Read this first at the start of any session on this project.

## Goal

A greenfield, MIT-licensed, open-source fan control application for Windows (Linux later,
macOS maybe) whose defining feature is a **node-graph editor with typed connections** —
Node-RED-style dataflow rather than a fixed list of curve widgets.

"Typed connections" is the thesis, not decoration. A temperature output physically cannot
be plugged into a PWM duty input; the type system forces an explicit transform node in
between. That makes fan configurations self-documenting, statically checkable, and
composable in ways a flat curve list is not.

Success ladder:

1. **Works on the reference machine** — real sensors, real PWM, safe.
2. **Parity** with the established closed-source Windows fan controller (middle goal).
3. **Surpass it** — the graph model, servo control, diagnostics, visualization, and
   cross-platform reach are where we go beyond.

Long-term intent is to support *everything*: every Super I/O, EC, GPU, AIO pump,
USB fan hub, and water-cooling loop we can get our hands on.

## Non-negotiable safety premise

Fans are the only thing between silicon and thermal damage. Every architectural decision
below is downstream of: **the fans must never stop because our software had a bad day.**

## Environment / context

- Dev box: `Noook`, Intel NUC10i7FNK, Windows 11 Pro N 26200, 64 GB, Samsung 970 EVO Plus.
  Reached over RDP. **This is not the target machine** — a NUC10 exposes only a
  BIOS/EC-governed CPU blower through an ITE EC and has no PWM headers. Fine for building
  and for mock-backend work; useless as a control testbed.
- Reference/target machine: **a different desktop, TBD** (see Open Questions #1).
- Repo: `C:\Users\camer\git\Personal Projects\open-fan`, primary branch `master`.
- Toolchain present: Rust 1.97.1 / cargo 1.97.1, Bun 1.4.2, Node 24.18.0, .NET 8.0.425,
  `gh` 2.83.2. HWiNFO64 is installed on the dev box.

## Decisions already made (don't re-ask)

| # | Decision | Reason |
|---|---|---|
| 1 | **Rust-native hardware access via PawnIO**, no .NET sidecar | PawnIO exposes a 4-function C ABI; binding it from Rust is trivial. Keeps a single self-contained binary, no .NET runtime, no second process on the control path, and keeps the safety-critical code in one language we control. |
| 2 | **React + React Flow** for the node editor | Most mature node-editor library, largest corpus of prior art for custom typed handles, and matches React already in use elsewhere (`react-smoothie`, `XLN-Control`). |
| 3 | **Tauri v2 (2.11.6 stable)**, not Tauri 3 | Tauri 3 is `3.0.0-alpha.1`. Nothing safety-critical ships on an alpha. Revisit when 3.x is stable. |
| 4 | **Bun** for the frontend, `bun.lock` committed | Standing preference across JS/TS projects. |
| 5 | **All decisions live in the Rust backend**; UI is a viewer/editor | The app must keep controlling fans with the webview closed or crashed. The UI is never in the control loop. |
| 6 | **MIT license**, clean-room | No decompilation, no reading disassembly or leaked sources of any closed-source product. Public file formats may be parsed for import. See "Clean-room discipline". |
| 7 | Target machine is a separate desktop, not the NUC | See Environment. |

## Clean-room discipline

This project is a clean-room implementation. The rules, so a future session cannot
accidentally poison the codebase:

- **Never** decompile, disassemble, or read decompiled output of any closed-source
  hardware-control product, and never copy from such output.
- Hardware knowledge comes from: chip datasheets, Linux `hwmon` drivers (GPL — read for
  *facts about hardware*, do not copy code into an MIT file), PawnIO's LGPL modules used
  as-is over their public ABI, and our own experimentation.
- The one place a competing product is legitimately referenced is **profile import**: its
  on-disk config is a documented-by-observation JSON file, and parsing a file format to
  interoperate is fine. Confine that to the importer crate and its tests.
- Docs, README, and this plan otherwise do not position the project relative to any
  specific competitor.

## Architecture

Single Rust binary. Cargo workspace, layered so the engine never depends on the UI and the
graph core never depends on hardware.

```
open-fan/
├── Cargo.toml                 workspace root
├── crates/
│   ├── of-units/              typed physical quantities + the port type system
│   ├── of-core/               graph model, node catalogue, evaluator (no I/O, no async)
│   ├── of-hal/                hardware traits: SensorSource, PwmSink, Discovery
│   ├── of-hal-mock/           simulated thermal plant — dev, tests, CI, demo mode
│   ├── of-hal-pawnio/         PawnIO FFI + Super I/O / EC drivers (Windows)
│   ├── of-engine/             tick loop, safety supervisor, watchdog, persistence
│   ├── of-config/             profile schema, versioned migration, importers
│   └── of-ipc/                backend↔UI DTOs, TypeScript emitted via ts-rs
├── src-tauri/                 Tauri shell: tray, window lifecycle, updater, commands
├── ui/                        React + React Flow + uPlot (Vite, Bun)
├── plans/
└── .github/workflows/
```

### The type system (the whole point)

Ports carry a physical quantity, not a bare number:

- `Temperature` (°C), `Duty` (0–100 %), `Rpm`, `Load` (%), `Power` (W), `Voltage` (V),
  `Current` (A), `Frequency` (Hz), `Throughput` (B/s), `Ratio` (dimensionless),
  `Boolean`, `Time` (s).

Connection validity is decided by the port type pair, enforced in **both** the editor
(React Flow `isValidConnection`, so an illegal edge cannot be drawn) and the backend
(graph validation on load, so a hand-edited or imported profile is rejected loudly rather
than silently misbehaving). The backend is authoritative; the frontend check is a
convenience.

Node families:

- **Sources** — sensor readings, constants, manual sliders, time/schedule, app/process
  state.
- **Transforms** — curve (piecewise linear / spline / flat), mix (max/min/avg/sum),
  offset, scale, clamp, hysteresis, low-pass filter, rate limiter, moving average,
  delay, comparator, switch/select, latch, PID.
- **Sinks** — PWM channel, pump channel, servo channel, notification, log.

Conversions that are physically meaningful get explicit nodes (a Curve is
`Temperature → Duty`). Conversions that are not, simply cannot be expressed.

### Evaluation

Directed acyclic graph, validated and topologically sorted on load, then evaluated at a
fixed tick (default 10 Hz, configurable). Cycles are rejected at edit time; feedback that
genuinely needs history goes through explicit stateful nodes (delay, filter, latch) that
carry state across ticks. Evaluation is pure and synchronous — sensor reads happen before
the tick, PWM writes after it — which makes the whole graph trivially testable against the
mock HAL with no hardware and no clock.

### Safety supervisor — "don't let the fans die"

Layered, each layer independent of the ones above it:

1. **Graph-level** — per-channel floor duty; a channel can never be driven below its floor.
2. **Critical temperature override** — a hard rule evaluated *outside* the graph. If any
   guarded sensor exceeds its critical threshold, affected channels go to 100 % regardless
   of what the graph says. Not expressible as a node, so it cannot be misconfigured away.
3. **Tick watchdog** — a thread independent of the engine. If a tick has not completed
   within N intervals, it declares the engine dead and applies the failsafe.
4. **Dying breath** — on panic, on unhandled fault, on Windows shutdown/logoff, and on
   normal exit: apply the configured failsafe to every channel we control. Default
   failsafe is **restore the chip's original automatic mode** (hand control back to the
   BIOS/EC, which is what the firmware is there for) with **100 % duty** as the fallback
   when restore is impossible. Both are per-channel configurable.
5. **External watchdog** — a small separate process that holds a handle to the main
   process and applies the failsafe if the main process dies without a dying breath (hard
   kill, OOM). This is what covers the cases layer 4 cannot.
6. **Auto-restart** — supervisor restarts the engine after a fault, with backoff, and
   writes a local crash report.

Ordering matters: the failsafe path must be reachable with no allocation, no async
runtime, and no lock that the faulting code might hold. That constrains its
implementation — keep the failsafe channel list in a pre-allocated, lock-free structure
populated at configuration time.

### Update safety

Auto-update is a controlled handoff, not a restart: apply failsafe → release hardware →
swap binary → restart → reacquire. An update must never leave a gap where nothing is
controlling the fans and the chip is still in manual mode.

## Findings / gotchas

### WinRing0 is dead — do not use it

The long-standing ring-0 shim used by nearly every Windows hardware tool
(`WinRing0x64.sys`) is flagged by Microsoft Defender as `HackTool:Win32/Winring0` /
`VulnerableDriver:WinNT/Winring0` under **CVE-2020-14979**. It is genuinely exploitable —
arbitrary physical memory access from an unprivileged caller — unmaintained since ~2010,
and its author abandoned it. Only the insecure 2008 build carries a valid signature; the
community-patched fork is unsigned and therefore unloadable under Secure Boot / HVCI.
Tools still depending on it require users to add AV exclusions, driver-blocklist
exceptions, or disable Memory Integrity. **That is not an acceptable install experience
and not an acceptable security posture. WinRing0 is permanently off the table.**

### PawnIO is the live successor, and it fits us perfectly

- A **signed, scriptable universal kernel driver**: the driver itself is generic and
  audited, and hardware-specific logic ships as sandboxed Pawn bytecode modules. The blast
  radius of a bad module is bounded by the interpreter, which is precisely what WinRing0
  lacked.
- LibreHardwareMonitor migrated to it in **v0.9.4**; OpenRGB, CapFrameX, OmenMon and others
  have followed. Independently, `Leyukaka/fancontrol-rs` (MIT OR Apache-2.0) is a Rust
  Windows fan controller built on PawnIO with **no** WinRing0 — direct proof the
  Rust-native path works. Useful as a reference for chip quirks; do not copy code without
  honouring its license.
- **Licensing works out cleanly for an MIT project.** The driver is GPL-2.0 but carries an
  explicit exception for *"independent modules that communicate with PawnIO solely through
  the device IO control interface"* — exactly what we do. `PawnIOLib` is **LGPL-2.1**, so
  dynamic linking from MIT code is fine. The official hardware modules are **LGPL-2.1**
  and ship pre-signed. We link dynamically, ship no PawnIO source, and stay MIT.
- **The C ABI is tiny** (`PawnIOLib.h`), all returning `HRESULT`:
  - `pawnio_version(PULONG)`
  - `pawnio_open(PHANDLE)`
  - `pawnio_load(HANDLE, const UCHAR* blob, SIZE_T size)`
  - `pawnio_execute(HANDLE, PCSTR name, const ULONG64* in, SIZE_T in_size, PULONG64 out, SIZE_T out_size, PSIZE_T return_size)`
  - `pawnio_execute_async(...)` with an `OVERLAPPED`
  - `pawnio_close(HANDLE)`
  - `*_win32` (BOOL/GetLastError) and `*_nt` (NTSTATUS) variants of each.

  Four `extern "system"` declarations and we have the hardware. Note `pawnio_execute_async`
  exists — worth using so a slow or wedged chip access cannot stall the tick loop.
- **Prebuilt signed modules already cover most of what we need**: `LpcIO` (Super I/O —
  Nuvoton NCT668x/679x, ITE IT87xx), `LpcACPIEC`, `IsaBridgeEC`, `DellSMM`, `IntelMSR`,
  `AMDFamily0F/10/17`, `RyzenSMU`, `SmbusI801`, `SmbusPIIX4`, `SmbusNCT6793`,
  `SmbusIntelSkylakeIMC`, `IntelPCHThermal`, `IntelMCHBAR`, `Nvidia`.

### PawnIO gotchas to design around

- **The signed edition enforces module signatures.** Custom Pawn modules for exotic
  hardware will not load unless they are upstreamed into `namazso/PawnIO.Modules` and
  signed, or the user installs the "Unrestricted edition". This directly constrains the
  "support everything eventually" goal: our exotic-hardware path is *upstream a module*,
  not *ship our own blob*. Plan for that lead time.
- **PawnIO is a prerequisite install**, not something we can bundle-and-forget. First-run
  needs a detect → explain → install flow, and a clear degraded read-only mode when it is
  absent.
- `fancontrol-rs` ships unsigned binaries and warns about SmartScreen. We should sign
  releases; budget for a code-signing certificate or accept SmartScreen friction and say
  so plainly in the README.

### Hardware facts about the reference machine (`Quasar`) — Phase 3

Surveyed 2026-09-21. This is the target machine the whole of Phase 3 was blocked on, and
it answers Open Question 1.

| | |
| --- | --- |
| Board | ASUS **ROG STRIX X570-I GAMING** (Mini-ITX), BIOS **4403** (2022-04-26) |
| CPU | AMD Ryzen 9 5950X (16C/32T) |
| RAM / OS | 64 GB, Windows 11 Pro 26200 |
| Super I/O | **Nuvoton NCT6798D** |
| GPU | NVIDIA RTX 3090 (GA102-A), 2 fan controls via NVAPI |

**Provisional header map** — *derived, NOT yet confirmed by Cameron.* Taken from the
existing fan tool's `userConfig.json`, which is the one competitor artefact the clean-room
rules allow us to read (a documented-by-observation config file, parsed for interop).
Confirm every row before writing anything:

| PWM channel | Tachometer | Label | Notes |
| --- | --- | --- | --- |
| `nct6798d/control/0` | `fan/0` | Chassis Fan | |
| `nct6798d/control/1` | `fan/1` | CPU Fan | |
| `nct6798d/control/4` | `fan/4` | **AIO Pump** | **Never stall-test. Never reduce casually.** A stopped pump on a 5950X is thermal runaway in seconds, and unlike a fan it has no airflow margin at all. |

Two further tachometers are exposed by the board's EC, not the NCT6798D, and appear to be
**read-only** (no matching control): `ec/fan/0` "VRM Heat Sink Fan" and `ec/fan/1`
"Chipset Fan". Being unable to control these is the correct and desirable outcome — they
are firmware-managed and should stay that way.

Only 3 of the NCT6798D's PWM channels are wired on this Mini-ITX board; indices 2, 3, 5, 6
exist in the chip but are not brought out to headers. Enumerating a channel the board does
not wire is a way to "successfully" control nothing, so channel enumeration must not simply
assume all 7.

### PawnIO on `Quasar`: installed, and the crate could not see it

PawnIO **2.0.1.0** is installed at `C:\Program Files\PawnIO`, service `PawnIO` running,
`Automatic` start. `pawnio_version` answers **2.0.0**. *The FFI written in Phase 1 works
against a real driver* — first confirmation, previously untested.

Two negative results worth keeping:

1. **`PawnIOLib.dll` is not on any search path.** It ships only in
   `C:\Program Files\PawnIO\`, which the installer does not add to `PATH`, and it is not in
   `System32`. So `libloading::Library::new("PawnIOLib.dll")` fails, and the crate reported
   the flatly false *"PawnIO is not installed. ... install it from https://pawnio.eu and
   restart."* on a machine where it was installed and running. Any backend trusting
   `is_available()` would have silently fallen back to the mock. Fixed by resolving the
   install directory explicitly. The locations we try, in order:
   - `PawnIOLib.dll` bare, so a copy beside our exe or on `PATH` still wins;
   - `HKLM\SYSTEM\CurrentControlSet\Services\PawnIO\ImagePath`, which here reads
     `\??\C:\Program Files\PawnIO\PawnIO.sys`; the DLL sits beside the `.sys`. Best source,
     because it points at the driver *actually loaded* rather than one merely installed;
   - the uninstall key's `InstallLocation`, under
     `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*` where `DisplayName` is
     `PawnIO`;
   - `%ProgramFiles%\PawnIO`.
2. **No hardware modules are installed.** The PawnIO installer ships the driver, the
   library, `PawnIOUtil.exe` and an uninstaller — and *no* `.bin` module blobs. `LpcIO`,
   which is what we need for the NCT6798D, comes separately from the
   `namazso/PawnIO.Modules` releases. So "load the module from the PawnIO installation
   directory" does not work out of the box, and first-run must treat a **missing module**
   as a distinct, separately-explained condition from a **missing driver**.
   `PawnIOUtil.exe` only signs/tests/runs `.amx` files; it is not a module installer.

### The Super I/O bus on `Quasar` is contended — four ring-0 drivers, two active writers

Found running concurrently at survey time:

| Driver | Owner |
| --- | --- |
| `PawnIO.sys` | ours |
| `AsIO2.sys` / `AsIO3.sys` (`Asusgio2`/`Asusgio3`) | ASUS Armoury Crate (`ArmourySocketServer`, running) |
| `inpoutx64.sys` | direct port I/O shim used by the existing monitoring/fan stack |

and, critically, **the existing fan control application is running and actively owns
`control/0`, `control/1` and `control/4`** — the exact three headers we want.

This has a consequence that is easy to miss and would quietly break the central safety
invariant: **while another controller holds the headers, the "existing configuration" that
`acquire()` is supposed to capture is not the firmware's.** It is the other tool's manual
mode and its current duty. Recording that and calling it "restore firmware control" would
be a lie — `release()` would hand back manual mode with nobody driving it, which is exactly
the state the failsafe design exists to prevent.

Therefore: **the other controller must be stopped, and the machine rebooted so the BIOS
curve is what is actually loaded in the chip, before the first `acquire()` is trusted to
capture restorable firmware state.** Two writers on the same PWM registers is also simply
unsafe regardless of what we record.

### Hardware facts about the dev box (`Noook`)

Recorded so a future session does not re-derive them: `Win32_Fan` returns three useless
`Cooling Device` stubs with null `VariableSpeed`/`DesiredSpeed` — WMI is not a fan control
path on any machine, confirmed here. `MSAcpi_ThermalZoneTemperature` returns two zones with
values in tenths of a kelvin (`3672` = 94.05 °C, and a bogus `100`). ACPI thermal zones are
coarse and partly garbage; treat them as a last-resort source, never a primary one.

## Plan / steps

- [x] **Phase 0 — Research & decisions.** Driver landscape, licensing, ABI, stack choices.
- [x] **Phase 1 — Scaffold.** Workspace, MIT license, README, `CLAUDE.md`, Tauri v2 shell
      with tray / single-instance / autostart / hide-on-close, React + React Flow editor
      enforcing the type system, generated TypeScript bindings, GitHub Actions CI and
      release workflows, `cargo-deny` licence and WinRing0 ban.
- [x] **Phase 2 — Graph core.** 16-node catalogue with time-aware stateful nodes; the
      `Engine` (ticks when told, reads no clock) and the `runner` control loop on its own
      thread; `of-ipc` DTOs with TypeScript generated from the Rust definitions; Tauri
      commands for catalogue / inventory / graph / snapshot; the editor wired to the
      backend document with a node palette, live readouts and validation errors shown
      against the nodes that caused them.
- [ ] **Phase 3 — Real hardware.** ← *current step, on the target machine.*
      Briefed in [`phase-3-hardware-bringup.md`](phase-3-hardware-bringup.md).
- [ ] **Phase 4 — Safety.** Supervisor layers 1–6, dying breath, external watchdog,
      auto-restart, local crash reports.
- [ ] **Phase 5 — Product.** Profile persistence and switching, logging + uPlot charts,
      tray mini-visualization, sparklines on nodes/edges.
- [ ] **Phase 6 — Distribution.** Signed releases, auto-update with controlled handoff.
- [ ] **Phase 7 — Parity & import.** Profile importer + takeover wizard; fill remaining
      parity gaps.
- [ ] **Phase 8 — Beyond.** Servo controllers, limit-cycle detection and auto-damping,
      case visualizer, firmware-behaviour documentation, Linux, profile sharing across a
      dual-boot system.

## Backlog (captured from the initial brief, not yet scheduled)

- Detection of limit cycling with automatic mitigation (add damping / slow rate of change).
- Case visualizer — sensor placement in the case and loop topology, possibly 3D.
- True servo controllers.
- Mini sparklines/readouts on each node and connection.
- Taskbar/tray mini visualization of status and history.
- Profiles shared across a dual-boot system (implies an OS-neutral profile format and a
  well-known shared location — worth designing the format for this from the start).
- Built-in explanation of what the firmware does to fans when we are not running: during
  reboot, after a crash, at POST. Can we read the board's firmware fan settings? Can we
  write them? Investigate per-vendor; likely partially possible via ACPI/WMI on some
  boards and not at all on others.

## Open questions for the user

1. **Which desktop is the reference machine, and how do I get at it?** I need the
   motherboard model, the Super I/O / EC chip, and the fan header map. Easiest path: run
   HWiNFO64 there and send me a sensor dump, or give me a shell on it. Everything in
   Phase 3 is blocked on this. *(Blocking for Phase 3 only — Phases 1–2 proceed regardless.)*
2. **Should the control engine eventually run as a Windows service?** A service would
   control fans at boot before any login and survive logoff, which is strictly safer. The
   cost is UI↔service IPC, an admin installer, and restricted access to some GPU sensor
   APIs from session 0. *Recommendation:* keep the engine in a library crate that can be
   hosted either way, ship in-process + autostart first, and add a service host in Phase 4
   once the safety layers exist. The scaffold is being built so this stays cheap.
3. **Code-signing certificate?** Unsigned releases mean a SmartScreen warning on every
   install and update for every user. *Recommendation:* budget for an OV/EV certificate
   before Phase 6; until then, document the friction honestly.
4. **How aggressive should the default failsafe be?** *Recommendation:* restore the chip's
   automatic mode where possible (hand back to firmware), 100 % where not. Both per-channel
   overridable. Confirm this matches your intuition before it gets baked into Phase 4.

## Things not to do

- **Do not use WinRing0**, or any fork of it, under any circumstance. See Findings.
- **Do not put the UI in the control loop.** If a decision requires the webview to be
  alive, it is wrong.
- **Do not decompile anything.** See Clean-room discipline.
- **Do not target the NUC (`Noook`) as the control testbed.** It has no PWM headers. Build
  and test against `of-hal-mock` here; validate control on the reference machine.
- **Do not adopt Tauri 3 while it is alpha.**
- **Do not allocate, lock, or await on the dying-breath path.**
- **Do not let an auto-update leave the chip in manual mode with nobody driving it.**
- Do not assume WMI (`Win32_Fan`) or ACPI thermal zones are usable primary sources — proven
  useless/garbage on the dev box.

## Progress log

- **2026-09-21** — Phase 0 complete. Established that WinRing0 is unusable and PawnIO is
  the correct foundation; confirmed its C ABI, licensing compatibility with MIT, and the
  catalogue of pre-signed modules. Confirmed dev box is a NUC and unsuitable as a control
  target. Resolved stack decisions 1–7 with the user. Pinned current stable crate versions.
  Wrote this plan.
- **2026-09-21** — Phase 1 complete. Nine crates, 47 tests, `cargo fmt`/`clippy -D warnings`
  clean, frontend typechecks under strict TS, Tauri shell builds (10m29s cold). Notable
  outcomes:
  - The type system went in stricter than originally sketched: connection requires exact
    quantity equality with **no** coercion, so `Load` and `Duty` do not interchange despite
    both being percentages. Pinned by an exhaustiveness test over every quantity pair.
  - Fault propagation is pinned by tests rather than convention — a dead sensor poisons a
    mix instead of being averaged away, a clamp cannot launder a NaN into a duty, and a
    channel whose fan node was deleted is still failsafed. Treat those tests as spec.
  - `ui/src/bindings/` is generated from `of-units` by `bun run bindings` and **committed**,
    so the frontend builds with no Rust toolchain and drift shows up as a reviewable diff.
    CI fails if it is stale.
  - `deny.toml` bans `winring0`-shaped crates outright, so the decision cannot rot.
  - The app icon is generated by `scripts/make-icon.mjs` rather than committed as an opaque
    binary; `bun run icons` regenerates every platform size.
- **2026-09-21** — Phase 2 part one. Node catalogue 7 → 16, with stateful nodes
  parameterised in *seconds* and `dt` passed in, so tuning survives a tick-rate change and
  a late tick integrates correctly. `of-engine` split into `policy` (pure) / `engine`
  (deterministic, clockless) / `runner` (the only threaded, clocked part). New invariant
  made structural: acquiring a channel registers a policy entry, because `apply_tick`
  iterates the policy to decide who gets failsafed — a held channel with no entry would
  have been skipped every tick. A sensor reading arriving as the wrong *quantity* now
  faults rather than being used. Persisted graph types emit TypeScript behind a `ts`
  feature. 76 tests.
- **2026-09-21** — Handed off to the target machine for Phase 3; see
  `phase-3-hardware-bringup.md`. Phase 2's UI wiring intentionally left unfinished so the
  hardware session is not also editing the frontend.
- **2026-09-21** — Phase 2 complete. The editor now reads and writes the backend's
  document rather than a local demo. Notes for later:
  - **The UI polls; it does not subscribe.** A subscription would make the control loop
    responsible for pushing to a consumer that can be slow, suspended or gone. Polling
    keeps the dependency pointing the right way, and a wedged window costs the engine
    nothing. 250 ms is plenty for a 10 Hz loop.
  - **Edits are staged and applied explicitly.** A half-wired graph is a normal state
    while building one, and not a state the engine should be asked to run. Apply returns
    every validation error at once, each attributed to a node so the editor can mark them
    in place.
  - **A node's ports come from the backend catalogue**, re-typed from the instance's own
    parameters. The editor has no independent idea of what a node looks like, so it cannot
    offer a connection the backend would reject.
  - `NodeDescriptor.template` is a complete, valid `NodeKind`, so a node dropped from the
    palette is never born faulted.
  - Gotcha for future sessions: `#[tauri::command]` on a `pub fn` **in the crate root**
    fails to compile — the macro's generated re-export collides with the definition
    (`E0255`). Commands live in `src-tauri/src/commands.rs` for this reason.
  - `OPENFAN_MOCK=1` forces the simulated backend even where real hardware exists. That is
    the safe way to exercise the UI on a machine you do not want to experiment on.
- **2026-09-21** — Parameter editor. Node parameters are described by a **backend-published
  schema** (`of-ipc::params`) rather than hand-written forms, for the same reason ports
  are: adding a node kind needs no frontend change, and the two cannot disagree about what
  is configurable. A spec's `key` is the serde field name, so applying an edit is
  `{ ...kind, [key]: value }` with no mapping table to drift.
  - A test asserts the spec keys **exactly** cover each variant's serialized fields, so a
    new field cannot end up invisible and un-editable, and a renamed one cannot leave a
    spec writing to nothing.
  - Choice options are round-tripped through the document format in a test, so a picker
    cannot offer a value the backend would reject on apply.
  - Picking a sensor sets the declared `quantity` alongside the id. They fault when
    mismatched, so setting them separately would make a dead node the normal outcome.
  - Gotcha: ts-rs maps Rust `i64` to TypeScript `bigint`, which a number input cannot
    consume. Editor-facing integer bounds use `i32`.
  - Canvas selection is held in React state, not read off React Flow. Rebuilding the
    canvas discards its selection flags, which would otherwise close the inspector on
    every keystroke.
- **2026-09-21** — **Generic nodes and type inference.** Most transforms do not care what
  they carry, so they now declare their ports as a *type variable* and the type is
  inferred from what they are wired to. Concrete types enter at the edges (a sensor reads
  a temperature, a fan takes a duty, a curve emits one) and propagate inwards.
  - Twelve node kinds lost their `quantity`/`input` parameter entirely. There is no type
    to configure and therefore no way to set one inconsistently with the wiring.
  - `of-core::infer` is union-find over variables with at most one concrete binding per
    class. Variables are scoped per node, so two Clamps using `T` are independent.
  - **Unresolved is not an error.** A chain of generic nodes with nothing attached is a
    normal half-built graph; the editor draws those ports hollow white and they lock to a
    colour the moment a connection decides them.
  - A new `TypeConflict` error is attributed to the *node* that would have to be two
    things at once, which is more useful than blaming one of its edges.
  - The editor calls `resolve_types` on each structural edit rather than reimplementing
    unification. Drag-time validity uses the last resolved map with the rule "undecided
    accepts anything"; the backend re-infers on apply and remains the authority.
  - Runtime agrees with inference: generic nodes carry their *input's* quantity through,
    so a value cannot reach a sink tagged as something it is not. Only `Constant` needs
    the inferred type handed to it, having no input to take it from.
  - Gotcha: rewriting `node.rs` wholesale silently dropped the `cfg_attr` ts-rs derives,
    which surfaced as `NodeKind: TS is not satisfied` from a *different* crate. Check the
    derives survived after any full-file rewrite.
  - Gotcha: leaving `tauri dev` running during a large refactor corrupts incremental
    artifacts — it rebuilds the same `target/` concurrently. Symptom is a link error
    about `unresolved external symbol anon...llvm...`; fix is `cargo clean -p <crate>`.
- **2026-09-21** — **Tachometers stay out of the graph.** Decided that a fan's tachometer
  is a separate sensor source rather than an output of its `FanOutput` node, so the graph
  remains a true DAG.
  - A tach reading is a *measurement*, not a return value: what was commanded and what
    actually happened can differ a great deal (a stalled fan reads quiet and is not
    cooling). Modelling it as an output of the command node conflates the two, and invites
    reasoning about it as instantaneous when it necessarily reflects an earlier duty.
  - The pairing is not 1:1 in reality either — a tach can exist on a header we do not
    drive, and a header can have none — so the loop-back model would encode a relationship
    the hardware does not have.
  - The association is still shown: the editor draws a dashed "same device" link between a
    fan output and any sensor node reading its channel's tachometer, derived from
    `ChannelInfo.tachometer` in the inventory. It leaves the *bottom* of both nodes, since
    data flows left to right and a shared-hardware link is not data.
  - Those edges are rendered, never stored. They are built in the render memo rather than
    in edge state, so nothing that folds the canvas back into the document can pick them up.
  - **Still missing: a Delay node.** Genuine feedback (stall detection raising duty) is a
    cycle, and the evaluator rejects cycles by design. The intended escape hatch is an
    explicit one-tick delay, which was listed in the original node families and has not
    been built. Until it exists, closed-loop-on-RPM control is not expressible.
- **2026-09-21** — **Delayed ports; the tachometer loop is not a cycle.** Corrected the
  previous entry's conclusion. A fan's speed output does *not* need a delay node to be
  fed back, because the loop is already broken by the hardware and by the tick ordering:
  we read sensors, then evaluate, then write duties, so a speed reading necessarily
  predates this tick's duty.
  - `PortSpec.delayed` marks an output whose value does not depend on this tick's inputs.
    `FanOutput.rpm` and `Delay.out` are the two today.
  - Evaluation is now two-phase: delayed outputs are produced *before* the topological
    pass, from measurements and stored state. Cycle detection skips edges leaving a
    delayed port. A loop with no delayed edge is still rejected — pinned by a test, since
    the escape hatch must not quietly disable cycle detection.
  - Stall detection (`fan.rpm → comparator → select → fan.duty`) now compiles with no
    delay node, and there is a test asserting the graph under test contains none.
  - `TickInput.tachometers` carries the channel→sensor map per tick rather than storing it
    in the document. It is hardware knowledge, so a fan moved to another header must not
    leave a saved profile reading the wrong tachometer.
  - A `Delay` node exists on its own merits (seconds-based, like every other stateful
    node) and can break loops that hardware does not already break.
  - Editor: delayed edges are drawn with a long dash and labelled "earlier tick" — still
    typed data, visibly not instantaneous. Distinct from the grey dashed same-device link.
  - Knock-on: layout now classifies nodes by their catalogue **category** rather than by
    counting ports. `FanOutput` has an output now, and port-counting put the sink in the
    middle of the graph.
