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

### Hardware facts about the dev box (`Noook`)

Recorded so a future session does not re-derive them: `Win32_Fan` returns three useless
`Cooling Device` stubs with null `VariableSpeed`/`DesiredSpeed` — WMI is not a fan control
path on any machine, confirmed here. `MSAcpi_ThermalZoneTemperature` returns two zones with
values in tenths of a kelvin (`3672` = 94.05 °C, and a bogus `100`). ACPI thermal zones are
coarse and partly garbage; treat them as a last-resort source, never a primary one.

## Plan / steps

- [x] **Phase 0 — Research & decisions.** Driver landscape, licensing, ABI, stack choices.
- [ ] **Phase 1 — Scaffold.** ← *current step*
      Workspace, MIT license, README, `.gitignore`, Tauri v2 shell with tray and
      hide-on-close, React + React Flow UI, GitHub Actions CI.
- [ ] **Phase 2 — Graph core.** `of-units` type system, `of-core` node catalogue and
      evaluator, `of-hal` traits, `of-hal-mock` simulated thermal plant. Fully tested with
      no hardware. This is where the differentiator gets built.
- [ ] **Phase 3 — Real hardware.** `of-hal-pawnio` FFI, PawnIO detection/install flow,
      Super I/O discovery, sensor read, PWM write, mode save/restore. Validate on the
      reference machine.
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
