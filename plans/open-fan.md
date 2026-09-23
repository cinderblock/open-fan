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
| `inpoutx64.sys` | direct port I/O shim, auto-start, owner unidentified — **not** FanControl (see below) |

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

### Talking to the NCT6798D: elevation, the ISA mutex, and the `LpcIO` ABI

Confirmed by experiment on `Quasar`, 2026-09-21. Chip identification is no longer derived
from another tool's config — we read it ourselves: **chip ID `0xD42B` at slot 0
(`0x2E`/`0x2F`), a Nuvoton NCT6798D.** Slot 1 is empty. Two independent sources now agree
on the chip.

**PawnIO requires an elevated caller.** Every call from an unprivileged process fails
`pawnio_open` with `E_ACCESSDENIED` (`0x80070005`). Not a permissions quirk to work
around — it is the design, and it means *OpenFan cannot read a single sensor without
running as administrator*. Consequences:

- First-run has a third distinct failure mode, alongside "no driver" and "no module":
  "not elevated". `PawnIoError::AccessDenied` now says so in words.
- This is real evidence for **Open Question 2 (run as a service)**. A service runs
  elevated in session 0 by definition and would remove the UAC prompt entirely, control
  fans before any login, and survive logoff. The counter-evidence — that some GPU sensor
  APIs are restricted from session 0 — is unaffected by anything found here. The
  recommendation in Open Questions still holds, with more weight behind it.

**Every `LpcIO` ioctl requires the ISA bus mutex**, `Global\Access_ISABUS.HTP.Method`
(`\BaseNamedObjects\...` in NT naming), and the module's documentation says so on all
seven entry points. This is not advisory. Super I/O access is index-then-data against a
chip with one index register, so an interleaved access from another process makes us read
*the wrong register* — and a plausible temperature from the wrong register is the worst
possible output, far worse than a failed read. Implemented in `of-hal-pawnio/src/isa.rs`,
held for the lifetime of an `LpcIo` so a whole batched read is one uninterrupted
transaction.

`WAIT_ABANDONED` is treated as successfully acquired: a third-party tool that crashed
while holding the mutex must not be able to lock us out of controlling the fans forever.

Note the limit — ASUS's `AsIO` driver holds a **kernel** mutex that user mode cannot
acquire, so this does not serialise us against Armoury Crate on the EC ports
`0x25C`/`0x25D`. Avoid contending for those rather than trying to lock them.

The module's published ABI, learned from its LGPL source's interface documentation (facts
about an interface, not its implementation):

| ioctl | in | out |
| --- | --- | --- |
| `ioctl_select_slot` | slot: 0 → `0x2E`/`0x2F`, 1 → `0x4E`/`0x4F` | — |
| `ioctl_find_bars` | — | — |
| `ioctl_pio_inb` / `ioctl_pio_outb` | port [, value] | value / — |
| `ioctl_superio_inb` / `ioctl_superio_inw` | register | value |
| `ioctl_superio_outb` | register, value | — |

Two things the module does **not** do for you: it never enters configuration mode (the
vendor unlock sequence is ours to send — `0x87` twice for Nuvoton, `0xAA` to leave), and
it restricts port access to the selected index/data pair, the ASUS EC ports, and base
addresses found by `ioctl_find_bars`. That restriction is a useful backstop: a bug in our
port arithmetic cannot reach an arbitrary I/O port.

### Modules: where they live, and whether to fetch them at runtime

`LpcIO.bin` taken from `namazso/PawnIO.Modules` release **0.2.11**
(`b3896a1cab0d808fca31fe2ebcae045d59dac690da87b17c858bb8da357eb45e`), installed to
`%LOCALAPPDATA%\OpenFan\data\pawnio-modules\`. Deliberately the *local* app-data
directory, not roaming: these are signed binaries for the hardware in this machine, and
roaming them onto another is pointless. Search order is our own directories first, then
the PawnIO installation, so a version we have tested beats one another installer dropped
in a shared location.

**Decision — runtime download is reasonable, with conditions.** Asked whether OpenFan
should fetch a missing module itself. It should, because the strongest objection does not
apply: PawnIO's signed edition verifies the module signature *in the kernel*, so a
tampered or corrupted blob simply will not load and we do not have to trust the transport.
Fetching from upstream also avoids us redistributing an LGPL-2.1 blob. The conditions:

- **User-initiated, never automatic.** A silent network fetch from a fan controller is
  surprising, and surprising a user is how trust in a tool with kernel access is lost.
- **Version-pinned, with a hash we ship.** "Latest" is not a dependency.
- **Never on the control path.** The engine must start, fail honestly, and keep any
  channels it already holds; a fan controller that blocks startup on the network has
  failed at its one job. Offline and locked-down machines are normal.
- Missing module stays a clean degraded state, not an error dialogue.

### Installing PawnIO for the user: what is actually available

Surveyed 2026-09-21 on `Quasar`. The Phase 0 note that PawnIO "is a prerequisite install,
not something we can bundle-and-forget" stands, but the options are better than that
wording suggests — the *mechanism* is solved, and what is left is a licensing question and
a consent question.

**Upstream ships three first-party install channels:**

| Channel | Identifier | Notes |
| --- | --- | --- |
| winget | `namazso.PawnIO`, **2.2.0**, released 2026-03-15 | Published by namazso. Manifest pins `PawnIO_setup.exe` SHA256 `1f519a22e47187f70a1379a48ca604981c4fcf694f4e65b734aaa74a9fba3032`, marks `Offline Distribution Supported: true`. |
| Chocolatey | `pawnio` 2.2.0 | Community-maintained. *Embeds* the setup exe rather than downloading it, and invokes it as `-install -silent`. |
| Direct | `github.com/namazso/PawnIO.Setup/releases` | Same signed `PawnIO_setup.exe`. |

**The installer has a documented silent mode.** `PawnIO_setup.exe -install -silent`. The
2.2.0 release notes record two things we must handle: CLI exit codes are **DOS errors, not
NTSTATUS** (changed in 2.2.0 — so the code we parse depends on the version we invoke), and
**silent mode returns `ERROR_SUCCESS_REBOOT_REQUIRED`** when a restart is needed. A driver
install that quietly requires a reboot and is treated as success is a first-run state we
would otherwise get wrong.

2.2.0 also notes it "is now possible to expose the device to non-administrators, although
not recommended" — relevant to the elevation finding above, but upstream advises against it
and so do we. The service-vs-UAC answer stays Open Question 2.

**The licensing is genuinely ambiguous and we should not guess.** The PawnIO *source* is
GPL-2.0-or-later with the IOCTL-interface exception (README confirms both, and notes the
exception "does not include programs that communicate with PawnIO over the Pawn
interface"). But the signed **official edition** is a separate distribution, and the two
package repositories disagree about it: winget's manifest says **"Proprietary
(Freeware)"**, Chocolatey's says **GPL-2.0** pointing at `PawnIO/COPYING`. Neither
`pawnio.eu` nor `PawnIO.Setup` states redistribution terms for the signed binary, and the
site offers "custom licensing" on request via `admin@namazso.eu`.

*Negative result: there is no published grant letting us redistribute the signed
`PawnIO_setup.exe`.* Chocolatey embedding it is precedent, not permission. **Action: ask
namazso directly before any plan that ships the bytes.** Until then, assume we may invoke
the official installer but not mirror it.

**Decision — user-initiated in-app install, not a bundled or silent one.** Same shape as
the module-fetch decision above, for the same reason and one more:

- **Not chained into the OpenFan installer.** Bundling a kernel-driver install into ours
  makes our installer a driver installer, with the AV-reputation and elevation
  consequences that implies, and forces the decision at the moment the user has least
  context. It also breaks if PawnIO is already present at a newer version.
- **Prefer winget when present** (`winget install --id namazso.PawnIO --exact`): the
  manifest is namazso's own and winget verifies the hash. Fall back to the pinned direct
  download, verified by **both** the SHA256 we ship **and** an Authenticode publisher
  check, then `-install -silent`. Never "latest".
- **Never silent from the user's point of view**, whatever switch we pass. The UAC prompt
  is not the consent; an explicit "OpenFan needs the PawnIO kernel driver — here is what
  it is, [Install]" screen is. Surprising a user is how trust in a tool with kernel access
  is lost.
- **Never uninstall it.** PawnIO is shared with LibreHardwareMonitor, OpenRGB, CapFrameX
  and others. Removing it on OpenFan uninstall is a bug in someone else's program.
- **Never on the control path**, and a missing driver stays a clean degraded state.
- Handle **reboot-required** as its own outcome, distinct from success and from failure.

**A fetcher is code, not bytes — and that is the whole point.** Shipping a tool that
*downloads and invokes* the official installer distributes none of namazso's binary, so
the redistribution ambiguity above never has to be resolved. This is what winget's own
manifest is (metadata plus a URL and a hash) and what every VC++/.NET bootstrapper does.
It changes nothing about our GPL/LGPL position, which was already clean.

**We likely need no separate helper binary.** OpenFan cannot read one sensor without
elevation, and first-run must already distinguish three states (no driver / no module /
not elevated). An `[Install]` action on that screen is nearly free. A standalone elevated
helper only becomes necessary if Open Question 2 resolves toward a service with an
unelevated UI — then the helper is the thing that owns the UAC prompt. Decide it there,
not here.

**Run-time, not install-time — the run-time path is a strict superset.** Install-time
chaining is a one-shot: it cannot handle PawnIO being removed later, nor version drift,
and it makes our installer network-dependent and admin-requiring. We need run-time
detection regardless, so chaining adds machinery without removing anything. If we ever
want it, it must call the same code.

**Do not hand-roll the driver install.** Creating the service and dropping the `.sys`
ourselves means we own every partial-install state, on an object whose failure mode is a
machine that will not boot. Invoke `PawnIO_setup.exe`; it is the supported path.

**Mechanics the fetcher has to get right:**

- GitHub release assets redirect cross-host to `objects.githubusercontent.com` — the
  fetcher must follow the redirect.
- A version-pinned asset URL **404s if upstream retags or deletes a release**. That is a
  clean failure with a real remedy: show the user the URL and let them sideload. Offline
  and locked-down machines are normal and must stay in the degraded read-only mode.
- **Defer to whatever installed it.** If PawnIO came from winget or Chocolatey, running
  `setup.exe` over it fights the package manager. Detect the source and offer that
  manager's upgrade instead.
- *Open question — the upgrade path from 2.0.x is undocumented.* 2.2.0 states only
  "Support upgrading from 2.1.0 without uninstall"; 2.1.0's notes say nothing about
  upgrading at all. So whether `2.0.1.0 → 2.2.0` is clean or needs an uninstall first is
  **unknown**, and `Quasar` is the machine that would find out. Do not assume in-place
  upgrade works for versions below 2.1.0.

`Quasar` currently runs **2.0.1.0** while **2.2.0** is current, so "installed" and
"installed at a version whose ABI we have tested" are different questions and first-run
must ask the second one.

### The other fan controller is also a PawnIO client — measured, not assumed

Re-surveyed 2026-09-21. The "existing fan control application" on `Quasar` is **FanControl
2.4.5** (`C:\Program Files (x86)\FanControl`, running as `FanControl.exe` plus a separate
`FanControl.Service.exe`), and the question of whether it can share the bus with us is
answerable by inspection rather than guesswork.

Scanning its `LibreHardwareMonitorLib.dll` (**0.9.6.0**) for driver and mutex identifiers:

| String | Present |
| --- | --- |
| `Global\Access_ISABUS.HTP.Method` | **yes** |
| `LpcIO`, `LibreHardwareMonitor.Resources.PawnIo.LpcIO.bin` | **yes** |
| `\?\GLOBALROOT\Device\PawnIO` | **yes** |
| `WinRing0`, `Ring0`, `inpout` | **no** |

Three consequences:

1. **Bus-level coexistence is already solved, and the name matches byte for byte.** LHM
   waits on the *same* `Global\Access_ISABUS.HTP.Method` that `of-hal-pawnio/src/isa.rs`
   creates. Two PawnIO clients each holding their own handle get their own interpreter
   instance, so both loading `LpcIO` is fine; the mutex is what keeps the index/data pairs
   from interleaving. Nothing more is needed here, and nothing less is acceptable — the
   mutex is not optional politeness, it is the whole mechanism.
2. **LHM embeds `LpcIO.bin` as an assembly resource** rather than fetching it. That is
   precedent that redistributing the LGPL-2.1 module blob is normal practice, which
   weakens (but does not settle) the "fetch from upstream to avoid redistributing"
   argument recorded above. Revisit if runtime fetch proves awkward offline.
3. **LHM does not use `PawnIOLib.dll`** — no such string — it opens the device path
   itself. PawnIO 2.2.0's release note about restoring "the old unused device path for
   compatibility with broken third party PawnIOLib implementations" is about exactly this
   class of client. We use the real `PawnIOLib` and are not in that category.

`inpoutx64.sys` is running and auto-start, but LHM 0.9.6 contains no reference to it, so it
is **not** FanControl's — the earlier attribution in the table above was wrong. Owner
unidentified; likely part of the ASUS stack or a leftover. It matters only in that it is a
port-I/O shim whose users may not take the ISA mutex at all.

**What the mutex still does not buy us.** It serialises *transactions*, not *ownership*.
PawnIO has no lease, claim or arbitration concept — nothing stops two processes from both
setting `control/N` to manual and writing duties, and neither one reports the tug-of-war.
FanControl currently owns `control/0`, `control/1` and `control/4`: the exact three headers
we want. So the conclusion from the contention survey is unchanged and is *not* a PawnIO
question — it is the policy question already settled under "Other applications are on the
bus": stop FanControl and reboot before the first `acquire()` is trusted to capture
restorable firmware state.

### Other applications are on the bus, and handling that is a product feature

The user's decision: **do not stop the other tools.** Detecting that another application
is contending for fan control, reporting it clearly, and offering to shut it down
*reliably* is wanted as a **high-priority feature**, not a bring-up inconvenience. This
lands in the backlog and shapes Phase 4.

That means coexistence has to be engineered, not assumed:

- The ISA mutex above is the mechanism for *register-level* coexistence, and is mandatory.
- It does **not** stop another tool from also driving PWM. Two controllers writing the
  same channel produces a tug-of-war neither reports.
- **Consequence for `acquire()` that has not gone away:** with another controller live,
  the state we capture is *its* manual mode, not the firmware's. Restoring that is not
  "handing back to firmware". Until the headers are genuinely unowned — realistically,
  after stopping the other controller and rebooting — `can_restore_firmware_control()`
  cannot honestly return `true` for this machine.

### The reference machine's fan loadout (confirmed by the user)

Of the three wired headers, **only two have anything plugged into them**, which changes
the risk calculus for first writes:

| Channel | Tach | What is on it | Observed |
| --- | --- | --- | --- |
| `control/0` | `fan/0` | **nothing** | 76.9 % commanded, **0 RPM** |
| `control/1` | `fan/1` | the machine's **only case/CPU fan** | 45.8 % → 1477 RPM |
| `control/4` | `fan/4` | **AIO pump**, deliberately held at a fixed slow speed | 23.5 % → 2150 RPM (closed-loop to a 2200 RPM target) |

Useful consequences:

- **`control/0` is the correct first write target.** An empty header cannot stall, cannot
  stop cooling anything, and has no thermal consequence whatsoever. It still exercises the
  entire read → record → restore → write path. There is no reason to make a first write on
  a loaded header.
- **`control/1` is the only fan actually cooling the CPU.** Treat its stall point as
  precious and approach it downward, slowly, only with the user present.
- **`control/4` is a pump and is off-limits for exploration.** It is *intentionally* slow,
  so "low RPM" here is not a fault to be corrected — and anything that reads the pump as a
  stalled fan and "helpfully" ramps or stops it is a bug with liquid-cooling consequences.
  A pump needs its own channel class, not fan heuristics.
- The 45.8 % → 1477 RPM and 23.5 % → 2150 RPM pairs are live cross-checks for the register
  decode, available without a side-by-side HWiNFO run.

### Step 2 outcome: a real, read-only backend

`of-hal-pawnio::SuperIoBackend` implements `SensorSource`, `OutputChannel`, `Backend` and
`Discovery`, and `src-tauri` selects it when the hardware is there. Verified on `Quasar`
through the `Backend` trait — the same path the engine uses, not a side channel:

```
backend: Nuvoton NCT6798D
9 sensors, 7 channels, can_restore_firmware_control: false
batched read: 0.4-0.6 ms   cputin=44.0  systin=47.0  fan/1=1478  fan/4=2156
```

**A whole batched read costs about 0.5 ms**, roughly 0.5 % of a 100 ms tick at the default
10 Hz. Chip access is not going to be what makes the control loop miss a deadline, and
there is ample headroom for more sensors, a slower chip, or a faster tick.

Control is deliberately refused. `acquire` and `set_duty` return an explanation rather
than `Unsupported`; `can_restore_firmware_control()` returns `false`. That is the required
order, not an oversight — a PWM register must not be written before its firmware
configuration is captured and proven restorable, and proving restoration means watching a
real fan with someone present.

Known gaps, recorded so they are not rediscovered:

- **`SensorKind` has no `Duty`.** So the PWM duty readback, which decodes correctly and is
  printed by the `nct-dump` example, cannot be exposed as a sensor. Mapping it to `Load`
  would be exactly the quantity confusion the type system exists to prevent — `Load` and
  `Duty` are both percentages and deliberately do not interchange. Adding
  `SensorKind::Duty` means touching `of-hal`, `quantity_of` in `of-engine`, the mapping in
  `src-tauri/src/commands.rs`, and regenerating bindings.
- **All seven PWM channels are enumerated**, though this board wires only three. Which
  channels a *board* brings out to a header cannot be discovered from the *chip*: an
  unwired channel accepts a duty perfectly happily and cools nothing. Tachometer pairing
  is what lets a user tell them apart. Naming belongs to configuration.
- **`min_reliable_duty` is `None` everywhere**, because the stall point has not been
  measured. A guessed floor is a fan that stalls at a duty we called safe.
- **The six configurable temperature sources and all voltages are still unexposed**, for
  the reasons under the decode section.

### Developer tooling added

Four read-only examples, all of which need elevation:

| Example | Purpose |
| --- | --- |
| `probe` | Is PawnIO there, what version, where are modules searched, does `LpcIO` load |
| `superio-scan` | Identify the Super I/O in both LPC slots |
| `nct-dump` | Capture the register space to a file, and decode it through the same functions CI tests |
| `read-sensors` | Drive the real backend through the `Backend` trait, as the engine does |

`nct-dump` is how the fixture in `crates/of-hal-pawnio/tests/fixtures/` was made. Anyone
adding a second chip should start by capturing one.

### Proof the app itself uses the real backend

`src-tauri` selecting the backend was the one piece written this session that the
`read-sensors` example does not exercise, so it was run for real. Elevated debug build,
25+ seconds, with the other two hardware tools still running:

```
INFO open_fan_lib::state: found real hardware backend=Nuvoton NCT6798D
INFO open_fan_lib::state: starting control loop backend=Nuvoton NCT6798D
```

No warnings or errors after that, and 0.2 CPU-seconds over the run. The engine ticked at
10 Hz against real silicon, reading real sensors, holding no channel and writing nothing —
`acquire` refuses, so there is nothing for the failsafe path to do yet.

**Coexistence verified under load.** With four readers on the LPC bus at once — the OpenFan
engine at 10 Hz, a separate `read-sensors` process, and both other vendor tools — every
read stayed consistent and took 0.4–0.6 ms, with no timeouts and no garbage values. All
three applications stayed responsive. The ISA bus mutex does what it claims.

Not yet proven, because it needs writes: that closing the window keeps control running,
and that quitting from the tray hands a header back to firmware.

### Environment differences on `Quasar` vs the dev box

- **Bun is 1.3.6 here, 1.4.2 on `Noook`, and the lockfiles are incompatible.** Bun 1.3.6
  cannot even parse the `lockfileVersion: 2` that 1.4.2 writes — `bun install
  --frozen-lockfile` fails with `Unknown lockfile version`, and a plain `bun install`
  silently rewrites the lockfile *down* to version 1. That downgrade must not be
  committed. Workflow on this machine until the versions are aligned: `bun install`, then
  `git checkout -- bun.lock`. Aligning the Bun versions is the real fix.
- `ui/node_modules` was absent; `bun install` is needed before `bun run build`.
- **The compute broker is not installed here.** `node ~/.claude/bin/cpu-slots.mjs` does not
  exist on `Quasar` (`MODULE_NOT_FOUND`), so the build guidance in `CLAUDE.md` applies to
  the dev box only. Plain `cargo build` on a 5950X is fast enough that this does not
  matter much.
- Running anything that touches hardware needs elevation, including `cargo test` if
  hardware-gated tests are ever added. The examples are all launched via
  `Start-Process -Verb RunAs`.

### Why the build runs `tsc` when Bun is the toolchain

`bun run build` is `tsc --noEmit && vite build`, and the `tsc` half is not redundant:
**Bun does not type-check, and neither does Vite.** Both strip types and bundle. There is
no `--typecheck` flag to reach for; it is a deliberate speed decision upstream.

Demonstrated rather than assumed, on `Quasar` with Bun 1.3.6:

```text
$ echo 'const n: number = "definitely a string"' > bad.ts
$ bun build bad.ts     ->  Bundled 1 module in 13ms
$ tsc --noEmit bad.ts  ->  error TS2322: Type 'string' is not assignable to type 'number'
```

Bun is doing three jobs in this repo — package manager, test runner, and (through Vite)
transpiler. Type *checking* is not one of them, and `tsc --noEmit` is the only thing
performing it.

That matters here more than in a typical frontend. `ui/src/bindings/` is **generated from
Rust**, so `tsc --noEmit` is what catches the bindings drifting out of step with the
backend types — the same drift CI guards against by regenerating them. Without it a
frontend that is type-incoherent with the engine builds and ships in silence, which is
exactly the failure the typed-port thesis exists to prevent.

**Do not drop `tsc` from the build to make it faster.** If the cost ever matters, the
replacement is a faster *type checker* (the native TypeScript port), not removing the
check.

### A link in the webview goes nowhere unless it is handed to the browser

Clicking React Flow's "React Flow" attribution badge did nothing. The badge is an ordinary
`<a target="_blank">`, and a Tauri webview has no answer for that: there are no tabs to open
one in, and WebView2 declines the new-window request rather than navigating. Every external
link in this app has the same problem — the badge was just the first one rendered.

Hiding the badge was the other option and is the wrong one here: `proOptions.hideAttribution`
exists, but xyflow's licence asks that it only be used with a Pro subscription, and OpenFan is
an MIT project that takes their work for free. So the link stays and is made to work.

The handling is one capture-phase click listener on `document` (`ui/src/external.ts`, installed
from `main.tsx`), which hands any `http(s)` link off to `tauri-plugin-opener`. Notes for anyone
adding a link later:

- The allowed URLs live in `src-tauri/capabilities/default.json`, not in the interceptor. A URL
  outside that scope is refused by the backend and the click does nothing *again* — visible only
  in a console the window cannot show. `cargo test -p open-fan` pins the attribution URL against
  the scope so that failure is caught at build time instead of by a user.
- Scope entries are globs, and `*` matches `/` as well, so `https://example.com*` also matches
  `https://example.com.evil.test`. Anchor the host: `https://example.com/*`, plus
  `https://example.com[?]*` if the URL carries a query string (a bare `?` in a glob is a
  single-character wildcard, which is the same hole).
- React Flow builds the badge's URL two ways — `?utm_source=attribution` in a release build,
  `/attribution` in a dev one — so testing this in `tauri dev` exercises a URL the release build
  never opens. Both are in the scope.

### Asking another controller to stand down, rather than killing it

Researched from published documentation only — no binary was inspected — and then tested.

**FanControl documents a way to be asked to exit.** `getfancontrol.com/docs` lists
`-e --exit`: *"Force the currently running instance to exit."* Run against its own
executable, which is how its documentation says to reach an instance that is already
running. Its wiki frames the command line as an automation surface for third parties.

**It works, where nothing else did.** On `Quasar` it exited in **4 seconds**. `WM_CLOSE` to
all eleven of its hidden windows and `WM_QUIT` to every GUI thread had each failed after
12. So `of_contention::stop` gained a gentlest rung, [`Politeness::Ask`], which runs an
application's own documented command before any window messages.

**And it hands the headers back.** Immediately after a clean exit both channels it had
been holding read **Smart Fan IV** — the board's own curve — not manual. That confirms
what its issue tracker reported anecdotally and matters a great deal: a *clean* exit
leaves nothing stranded, while termination would have left the CPU fan and the pump
frozen at whatever duty it last wrote. It is the difference between the gentlest rung and
the harshest, made concrete.

**Only fill `stop_command` in from published documentation.** A flag found by experiment
or by inspecting a binary is not a contract, and guessing at one on a program that holds
someone's fans is not reasonable.

**What could not be learned from public sources**, and was not pursued because it would
need decompilation: the IPC transport behind `FanControl.IPC.dll` (evidently gRPC, from
user-posted stack traces in public issues), its endpoint or schema, and whether `-e` stops
the newer FanControl *service* as well as its client. None of that is needed — the
documented command is enough.

### Taking over from another fan controller — measured, on `Quasar`

Established 2026-09-22 by doing it, not by reasoning about it. Implemented in
`of-contention` (process side) and `SuperIoBackend::ownership` /
`restore_firmware_mode` (chip side), rehearsed by the `takeover` example.

**A reboot is not required, and the earlier claim that one was is withdrawn.** It was
asserted from reasoning and was wrong. A firmware mode's configuration — curve points,
temperature source, thresholds — lives in registers the mode *selector* does not touch, so
it survives a trip through manual mode no matter who made the trip. Writing the mode
register back restarts the board's own curve with the board's own settings. Channel 0 had
already shown this four times; the takeover run then did it on six channels at once, and
all six were curving within seconds. **The chip does not record who wrote its mode
register, so a channel another application left in manual is not a special case.**

**Graceful shutdown of a third-party tray application is not reliable.** FanControl
survived both `WM_CLOSE` to all of its windows and `WM_QUIT` to all of its GUI threads,
12 s apiece. It is tray-minimised with `MainWindowHandle = 0`, eleven hidden top-level
windows and obfuscated single-character class names — there is nothing to politely close
and no contract that it would honour one. `of_contention::stop` therefore escalates and
*reports which rung it reached*, because an application that exits cleanly may hand its
channels back and one that is terminated certainly did not.

**FanControl does not re-assert the mode register while running.** It writes `Manual` once
when it takes a channel and writes only duties thereafter. So writing a firmware mode
takes the channel away from it *without stopping it*, and it does not fight back — the
chip's own algorithm simply overrides its duty writes. Two consequences:

- Restoring firmware control never needs the other application stopped.
- **Driving a channel ourselves still does.** Our control needs `Manual` plus our own duty
  writes, and its duty writes go to the same register — that is a genuine fight, and the
  chip has no arbitration to decide it. Stopping it remains a precondition for *control*,
  just not for *handing back to firmware*.

**A lead worth following: `FanControl.IPC.dll` ships in its install directory**, which
suggests a documented external-control surface. If it has one, asking it to stand down is
enormously better than killing it. Research this from public documentation only — **do not
decompile it**; the clean-room rules apply to it exactly as to any other closed product.

**The "is something actively driving this channel" heuristic is one-sided.** A duty that
moves while we write nothing proves another controller is live. A duty that holds still
proves *nothing*: a controller at a stable temperature writes the same byte every tick.
The first version of the takeover report asserted the strong form and confidently labelled
a CPU fan that FanControl was demonstrably driving as "abandoned, nothing is driving it".
The only conclusive test for the still case is to write a different value and see whether
it is stomped, which means writing to a channel we do not own — not something to do
silently behind a diagnostic.

**Incident, recorded because the lesson is process rather than code.** The takeover flow
has a guard refusing to restore firmware control while a controller is still running,
precisely to avoid starting a tug-of-war. It was written, silently failed to apply, and
was committed and run missing — the run then did exactly what it forbids. No test would
have caught it; verifying that the edit landed would have. Two follow-ups:

- **Move the takeover sequence into a library crate.** `cargo test` does not run
  assertions inside examples, so as it stands this flow has no coverage at all. That must
  happen before it is wired to a button.
- The side effect was benign but real: six channels went to the BIOS curve, including an
  AIO pump deliberately held at 23 % which jumped to 100 %. A takeover that changes a
  user's fan behaviour must say so *before* acting, not after.

### Update signing: keys, where they live, and what they protect

Generated 2026-09-22.

| | |
| --- | --- |
| Public half | compiled into `of_service::update::PUBLIC_KEY`, committed. **Not secret** — it is what users verify *against*. |
| Private half | repository secret `TAURI_SIGNING_PRIVATE_KEY` |
| Password | repository secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, 43 random characters |
| On this machine | `%USERPROFILE%\.openfan-signing\` — **outside the working tree**, and verified absent from anything tracked |

Key ID `D110467E2F9527DB`.

**Back up the private key and its password somewhere durable.** Losing them means no
future release can be signed, and an unsigned release is one that no existing installation
will accept — every user stops updating, permanently, with no way to recover except
manually reinstalling a build carrying a new key. The GitHub secret is *write-only*: it
cannot be read back out, so the copy in `%USERPROFILE%\.openfan-signing\` and whatever
backup exists are the only copies.

**The public key is deliberately not configurable at runtime.** It is the single thing
between "the service installs an update" and "the service installs whatever an attacker
served", so it must not be reachable from a request, a configuration file, or an
environment variable read at start-up. The build-time override exists only so a fork can
sign with its own key.

**Two formats, both accepted.** `tauri signer` emits base64-wrapped minisign files — the
public key is base64 of a two-line `.pub`, a signature is base64 of a four-line
`.minisig`. `minisign-verify` wants the unwrapped text. Getting that wrong does *not* fail
loudly: it fails as "signature verification failed" on a perfectly good release, which
looks exactly like an attack and sends you hunting in the wrong place. `update::keys`
tries both rather than guessing from shape, so a release signed with plain `minisign`
also verifies. The project's real public key is a test fixture, so if it ever stops
parsing that is a test failure rather than a broken release.

**A signature alone is not enough, and this is the interesting part.** It proves the bytes
are ours; it does not prove they are the version the feed announced — and the feed is a
plain JSON file whoever serves it controls. An attacker who can tamper with or merely
*replay* the manifest could announce `9.9.9` while serving a genuinely signed older
installer: the signature verifies, the version comparison passes, and the machine is
rolled back to a build whose flaws are known.

So CI signs with `--app-version`, which writes the version into the signature's **trusted**
comment — covered by the signature, therefore not editable without invalidating it — and
the service checks the manifest's claim against it. A signature carrying no version is
refused rather than accepted, because treating "no claim" as "any claim" makes the check
bypassable by deleting it.

**Signed as an explicit CI step**, not via `createUpdaterArtifacts`, which insists on a
`plugins.updater` configuration for a plugin we deliberately do not use. The service does
the updating with its own key; configuring an unregistered plugin would be pretending
otherwise. CI also fails early when the signing secret is missing: a release that quietly
ships unsigned is worse than one that does not ship, because the service refuses
unverified code and those artifacts would reach users who then silently never update.

**Verified end to end**, against the actual 4 MB installer this repository builds: key
parses, signature verifies, trusted comment vouches for the declared version. The test
skips when nothing is built, so it is free on a clean checkout.

### Self-update: two paths, and why the silent one is the constrained one

Implemented 2026-09-22, after the service made a genuinely prompt-free update possible.
Self-updating is a required feature, and so is being able to do it silently.

| Path | Who runs the installer | Prompt | Default |
| --- | --- | --- | --- |
| **Silent** | the service, as LocalSystem | none | **off** — opt-in |
| **Prompted** | the window, via `ShellExecuteW` | one UAC dialogue | on |

Both install the same signed artifact; only the prompt differs. That is a choice a user
should get to make, and the interface offers it as one.

**The silent path is the most dangerous capability in the product.** A LocalSystem service
that downloads and executes code outranks one that spins fans by a wide margin. So the
rules, all of which are load-bearing:

- **The service decides what it installs.** Feed URL and verifying key are compiled in
  (overridable at *build* time only). Nothing arriving over the pipe can supply a URL,
  file, version, signature or key. A caller may say "check now" and "install what you
  found" — that is the whole of its influence. Demonstrated: `applyUpdate` against a
  service that has found nothing answers *"no update has been found; check first"*.
- **Signature before execution, on both paths.** Verification stays in the service even
  for the *prompted* path, because an unelevated process could have the file substituted
  underneath it between check and launch.
- **Strictly newer only.** A silent downgrade is how a build with a known flaw gets put
  back on a machine that had already moved past it.
- **Opt-in, stored in `%ProgramData%`**, so an unprivileged user cannot enable unattended
  LocalSystem installs for the whole machine.

Every rule that could let the wrong thing be installed is a pure function in
`of_service::update::decide`, tested over downgrades, unparseable versions, plaintext
URLs, absent signatures and prerelease ordering. The update decision is not something to
leave untestable inside a network call — the same lesson as the takeover guard.

**With no signing key, nothing installs.** `PUBLIC_KEY` is empty until releases are
actually signed, `verifiable` reports `false`, and both paths refuse. Refusing loudly is
the right posture for an unsigned project, and the interface says so rather than silently
doing nothing. **Generating a key and signing releases is the remaining work** before
either path does anything on a user's machine.

**Prompted is still `ShellExecuteW`, not `CreateProcess`.** Only the former honours the
installer's manifest and elevates; a plain spawn fails with `ERROR_ELEVATION_REQUIRED`,
which is a baffling way to discover the difference.

**Handing the fans over needs nothing special**, which is the dividend of the service. The
installer stops the service, stopping runs the dying breath, the fans return to the
board's curve, the new service starts and picks them up. Observed across a real upgrade.

### Auto-update: what is wired, and what must land first

Surveyed 2026-09-22, after the first installer was produced.

**Nothing is wired today.** No `tauri-plugin-updater` dependency, no
`bundle.createUpdaterArtifacts`, no `plugins.updater` public key or endpoints, and no
`latest.json` published anywhere. The release workflow *does* already pass
`TAURI_SIGNING_PRIVATE_KEY` and its password, so the intent was there; nothing consumes
them yet. The mechanical work is small: generate a keypair, set
`createUpdaterArtifacts: true`, add the plugin and an endpoint, publish `latest.json`.

**Auto-update is independent of the takeover work.** They are different transitions with
different mechanisms and neither gates the other; an earlier draft of this section wrongly
implied otherwise.

**It is also not gated on Phase 4.** The handoff the architecture calls for is *already
implemented for a normal exit*: `ControlThread`'s `Drop` runs `Engine::shutdown`, which
applies the dying breath to every acquired channel, and `EngineHandle`'s `Drop` joins that
thread — so any ordinary process exit, including tray-quit, hands channels back before the
process goes away. What Phase 4 adds is the **hard-kill** case, which no amount of `Drop`
can cover.

**Both earlier worries are now answered, and the service answered them.**

**Can an unelevated app launch the elevated installer?** Yes. `tauri-plugin-updater` uses
`ShellExecuteW` with the `open` verb, which honours the target's manifest and elevates —
read from its source rather than assumed. Had it used `CreateProcess`, this would have
failed outright with `ERROR_ELEVATION_REQUIRED`, which is exactly what happened in this
session when a non-elevated shell tried to launch a `requireAdministrator` binary. So
**self-update works, at the cost of one UAC prompt per update.**

**Does the swap strand the fans?** No, and this is now observed rather than reasoned
about. The window holds no channels — the service does — and the installer stops the
service before replacing files. The service log across a real upgrade:

```text
stop requested; handing channels back
channels handed back; stopping
    <- files replaced here
found real hardware -> starting control loop -> running
```

The updater calling `std::process::exit(0)` immediately after `ShellExecuteW`, which runs
no destructors, therefore does not matter: the process it kills owns nothing.

**A bug this uncovered.** `--install` called `create_service` unconditionally and failed
with "service already exists" on any reinstall. Since the preinstall hook stops the old
service first, that failure would have left a *stopped* service pointing at a replaced
binary — fan control silently gone after an update. Registration is idempotent now and
re-points an existing entry at the current directory.

**The prompt-free alternative, if it is ever worth it.** The service runs as LocalSystem
and can already write to its own install directory, so it could apply updates itself with
no prompt at all — which is how Chrome and similar products do it. Not done, and not
obviously worth doing: it means a service that downloads and executes code from the
network, which is a much larger security surface than a user clicking through one UAC
dialogue. Tauri's minisign check would still gate it, but the blast radius of a mistake is
different. Revisit only if per-update prompts become a real complaint.

**Two adjacent facts worth not confusing.** Tauri's signing (minisign) authenticates the
*update payload*, and is what stops a malicious update being accepted. **Authenticode** is
what stops SmartScreen warning on every install and every update, and we have neither.
They are different problems with different price tags; Open Question 3 is about the
second one.

**Autostart is incompatible with `requireAdministrator`.** `tauri-plugin-autostart` uses
the `HKCU\...\Run` key, which Windows will not use to launch an elevated application. The
plugin is initialised but never enabled, so nothing is broken right now — it will be the
moment someone turns it on. The fix is a scheduled task registered with *run with highest
privileges*, which also has to be created by an elevated installer. This is a knock-on of
requiring administrator and belongs with the service question (Open Question 2): a service
runs elevated at boot and makes both problems disappear.

**One thing that already works:** the PawnIO module cache lives in
`%LOCALAPPDATA%\OpenFan\`, outside the install directory, so it survives updates and
uninstalls. A PawnIO *version* change is a separate matter and first-run already
distinguishes "installed" from "installed at a version we have tested".

### The service: implemented, and what it settled

Built 2026-09-22 and verified on `Quasar` by installing it. Option **D** from the
elevation table above; the other three are now history rather than alternatives.

```
Service   OpenFan fan control   LocalSystem, Automatic, C:\Program Files\OpenFan\openfan-service.exe
Window    open-fan.exe          asInvoker — no UAC prompt, ever
Between   \.\pipe\OpenFan      newline-delimited JSON
```

**What it dissolved.** Elevation, autostart and boot-time control were three problems with
one answer. The window asks for nothing, so `requireAdministrator` is gone and with it the
conflict that made "start with Windows" impossible. The service starts before any login
and keeps running across logoff.

**What it cost.** Nothing safety-critical moved: `of-engine` was always a plain library
with no opinion about its host, which was the point of writing it that way. Two new
crates — `of-rpc` for the boundary, `of-service` for the host — and the takeover flow
moved from the Tauri layer to the service, because that is where the hardware is now.

**Two protocol decisions worth not undoing.** There is no request that stops fan control,
releases channels or shuts the service down — stopping it is an administrative act through
the service control manager, because a fan controller any user process can silently switch
off is not a fan controller, and a test pins the request surface. And a malformed request
is answered rather than dropped, so a client can tell a protocol bug from an absent
service.

**The pipe's DACL is a real trust boundary.** LocalSystem and Administrators get full
access; authenticated users get read and write, because otherwise an unelevated editor
cannot talk to us at all. So any process running as the logged-in user can command the
fans — the same authority it already has over that desktop, and the price of not prompting
for administrator every time someone looks at a fan curve. Bounded by the two decisions
above.

**Stopping is a safety operation.** `STOP`, `SHUTDOWN` and `PRESHUTDOWN` all mean the same
thing; the host is dropped *before* `STOPPED` is reported, so the dying breath actually
runs; `StopPending` asks for 30 seconds first. The uninstaller stops and removes the
service **before deleting any file**, because deleting a running service's binary would
leave fans wherever OpenFan last set them with nothing responding to temperature.

**The bug that only a service could have.** First start as LocalSystem found no hardware
and quietly used the simulated backend on a machine with real fans. The PawnIO module
cache was per-user, and a service's `%LOCALAPPDATA%` is
`C:\Windows\System32\config\systemprofile\AppData\Local` — not the logged-in user's. Module
discovery now searches `%ProgramData%\OpenFan\pawnio-modules` first. **Any path derived
from the "current user" is suspect in this codebase now**; the engine's user is
LocalSystem.

**Still open.** The installer does not place the PawnIO module, so a fresh machine still
needs one — that is the runtime-fetch item already in the backlog, and it must write to
`%ProgramData%` rather than a user directory. The tray application's own autostart is
still registered through the Run key, which now works again but starts only the *window*;
the service needs no such help.

### When to elevate: the options, and what the neighbours do

Requiring administrator is **normal for this class of application** — nothing that reads a
Super I/O can avoid it. What varies is *when* the prompt happens, and that is a real
design choice rather than a detail.

| | Mechanism | Prompt | Autostart | Cost |
| --- | --- | --- | --- | --- |
| **A** | Manifest `requireAdministrator` *(current)* | every manual launch | **broken** | none |
| **B** | `asInvoker`, relaunch elevated on demand | when the user opts in | **broken** | app must work in two modes |
| **C** | Scheduled task, *run with highest privileges* | **none at logon** | works | installer must create the task |
| **D** | Windows service + unelevated UI | **none after install** | works, and at boot | IPC, service host, installer |

**A is where we are.** Honest and simple: PawnIO refuses a handle to an unprivileged
process, so an unelevated OpenFan cannot read one temperature. Without the manifest it
would fall back to the simulated backend and show a plausible, fictional machine.

**B** is worth naming only to reject it. Starting unelevated and offering to restart
elevated means the app has to be coherent in a state where it can see no hardware at all —
every panel, every sensor list, every graph. That is a lot of surface for a state nobody
wants to be in, and it still cannot autostart.

**C is the cheap next step and the one that unblocks autostart.** A task registered with
*run with highest privileges* launches elevated at logon with **no prompt**; the installer
is already elevated, so it can create it. Confirmed necessary:
`tauri-plugin-autostart` uses `auto-launch`, which writes only to
`HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run` and has no Task Scheduler path — and
Windows will not launch an elevated application from that key. If the Start-menu shortcut
also triggers the task rather than the exe, the manual launch stops prompting too.

**D is the destination**, and it is Open Question 2 already. A service dissolves three
problems at once — elevation, autostart, and controlling fans *before any login* — and it
survives logoff. The engine is already a library crate precisely so it can be hosted
either way.

**What the neighbours do:** the tool-shaped ones (FanControl, LibreHardwareMonitor,
HWiNFO) require administrator and offer a scheduled task for startup — pattern A plus C.
The more commercial ones (MSI Afterburner, Argus Monitor) install a service — pattern D.
Both are normal; D is the more polished.

**One consequence of A worth being explicit about.** Between boot and the moment a user
logs in and accepts the prompt, OpenFan is not running and the fans are managed by the
board firmware. That is *fine* — and it is fine specifically because releasing hands
channels back to the firmware curve rather than to a frozen duty. The elevation model and
the one-way release decision hold each other up; changing either without the other leaves
a gap at boot.

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
- [~] **Phase 3 — Real hardware.** ← *current step, on the target machine.*
      Briefed in [`phase-3-hardware-bringup.md`](phase-3-hardware-bringup.md).
- [ ] **Phase 4 — Safety.** Supervisor layers 1–6, dying breath, external watchdog,
      auto-restart, local crash reports.
- [ ] **Phase 5 — Product.** Profile persistence and switching, logging + uPlot charts,
      tray mini-visualization, sparklines on nodes/edges.
- [ ] **Phase 6 — Distribution.** Signed releases, auto-update with controlled handoff.
- [~] **Phase 7 — Parity & import.** Importer and the three-state onboarding done;
      remaining parity gaps unfilled. See "Meeting a machine that already has fan
      software" below.
- [ ] **Phase 8 — Beyond.** Servo controllers, limit-cycle detection and auto-damping,
      case visualizer, firmware-behaviour documentation, Linux, profile sharing across a
      dual-boot system.

## Meeting a machine that already has fan software

A new installation lands in one of three states, and each needs a different answer:

| State | Answer |
| --- | --- |
| Nothing else installed | A preset generated from the hardware actually present |
| Installed but idle | Import its configuration; switch off its autostart |
| Running now | The above, plus stand it down (takeover) |

### What the on-disk format turned out to do

Established by reading real files, which is the one place the clean-room rules permit a
competitor to be referenced. Fixtures live in `crates/of-config/tests/fixtures/`.

- **A field called `Percent` does not always hold one.** A fixed-speed curve on the
  reference machine holds `2200`, and an older backup of the same curve holds `1500`.
  They are RPM. Clamping either into range produces a plausible profile that runs a pump
  flat out, so a value that cannot be a duty is refused and named. This is the reason
  `duty_percent` exists and why it returns `Option`.
- **The section key moved** from `Main` to `FanControl` between versions, so the key
  present selects the layout — not the version number, which is not reliable:
  `backup_V255_userConfig.json` declares version 265 internally.
- **Calibration rows gained a third column**; the first two never changed.
- **Temperature sources are numbered by position**, and ours are keyed by what the input
  measures. There is no faithful translation, so every imported curve carries a note
  asking for the binding to be checked. Resolving this properly needs a positional map
  per chip and is not yet worth the risk of guessing.

### Calibration is the prize

Those files carry a measured duty-to-speed table per fan, including where the fan stops.
That is a measurement we would otherwise repeat by driving a fan towards stall on a
machine trying to stay cool. `Calibration::found_the_stall` refuses to infer it from a
table that never reached zero — no stall reading means the table says nothing about how
slowly that fan can safely run.

### Autostart is a separate kind of obstacle

A rival installed with autostart but not running is invisible to `detect`, so a survey
reports all-clear and the fight starts at the next reboot with nobody watching. It is
surveyed as a *future* obstacle rather than folded into "is it running".

Two things that would have made it quietly wrong, both now fixed and tested:

- As LocalSystem, `HKEY_CURRENT_USER` is the service's own profile. The signed-in user's
  `Run` key is reached through `HKEY_USERS` instead.
- A Startup shortcut stores its target inside a binary structure. Matching the whole blob
  recognised the application and then failed the "is it still installed" check, so a real
  entry would have been detected and dropped.

### Where the work is split, and why

The import runs in the **window**, not the service: it is pure computation on a file the
user can already read, so the LocalSystem service never opens a path a client named.
Switching off an autostart entry goes to the service **by index into a survey the service
holds** — never by path, or any client could name anything for a privileged process to
delete. The service is asked only for what needs elevation: what is running, and what is
scheduled to start.

### Finding an installation

There is no reliable record. The uninstall registry entry — the documented place to look
— is **absent on the reference machine**, which has the software installed and working.
So discovery goes by evidence, strongest first: a running process knows its own path, a
startup entry names one, and failing both, the usual install directories. That last case
is the common one rather than a fallback.

### Verified on the reference machine

- The service examines **142 startup entries** and finds 0 fan controllers, which is the
  evidence that it really reads `System32\Tasks` — unreadable without elevation, and the
  reason that half of the survey cannot move into the window.
- Discovery finds the real configuration at its install path, with no process and no
  startup entry pointing at it: the "installed but idle" case, end to end.
- The inventory ids the importer translates onto are `nct6798d/pwm/0..6`,
  `nct6798d/temp/systin` and `nct6798d/temp/cputin` — the same shape the importer tests
  assume, so those tests model this machine rather than an invented one.
- **Not verified:** the UI rendering, and switching an autostart entry *off*, because
  this machine has no rival autostart entry to switch off.

### A response that could not be encoded

`Response` is internally tagged, and **serde cannot serialize a tagged newtype variant
wrapping a sequence**. `AutostartSurvey(Vec<_>)` failed at serialization time, so the
server wrote nothing and closed the pipe; the client reported only "the connection
closed". List-carrying variants must be struct variants. Every response variant is now
encoded by a test — requests had that test and responses did not, which is why it shipped.

Related: a panicking handler used to drop the connection with nothing logged. `converse`
now catches, logs and answers, so the next failure names itself.

### Getting started is a modal, not a sidebar section

It shipped as a section of the 310 px sidebar and did not work there. Every paragraph in
it exists to say what a button will do *before* it is pressed — a startup entry being
removed, another tool's configuration being translated — and at that width those wrapped
into a column nobody reads. Consent nobody reads is not consent, so it takes the window.

- A native `<dialog>` opened with `showModal()`, for the focus trap, the inert background
  and Escape. It renders from `App`, not `Sidebar`, so the scrolling column cannot clip it.
- It opens itself **once**, when the document loads with no nodes — the one state where an
  empty canvas and a node palette are not an answer. Anything already configured is left
  alone; a welcome screen to dismiss every launch is the thing being avoided. Afterwards
  it is a **Getting started** button in the sidebar.
- The survey runs on open rather than on mount, so reopening re-reads the machine, and an
  application opened only to look at a graph never walks the registry.
- Applying a preset or an import **closes** it, and the outcome sentence moves to a
  notice strip over the canvas. The sentence says to go and check which temperature each
  fan follows before turning control on — the modal is what covers the graph, so keeping
  it open made the instruction undismissable and unfollowable at the same time. A *failed*
  apply is the opposite case and stays in the modal: nothing loaded, the editor behind is
  unchanged, and the reason belongs beside the button that caused it.
- That strip was the connection-rejection notice, which was red and the only thing of its
  kind. It is now `.notice` with `--rejected` and `--applied` tones; the sentence says
  which it is, so the colour is never carrying the meaning alone.
- **Still not verified visually** — the dev box has no rival installed and the UI has not
  been seen rendered on `Quasar`.

### Not done

- No file picker yet, so a configuration outside the usual places cannot be pointed at.
- Start/stop thresholds have no equivalent node; the numbers are carried into
  `Calibration` rather than half-implemented.
- Curve kinds beyond a line and a constant are named in a note, not approximated.

## Backlog (captured from the initial brief, not yet scheduled)

- ~~**Autostart cannot work while the app requires administrator.**~~ **Resolved by the
  service.** The window dropped to `asInvoker` when the service took the hardware, so the
  Run key works again and there is a toggle for it. The scheduled-task workaround is not
  needed: what actually had to start at boot was fan control, and that is the service.
- ~~**Publish `latest.json` at the feed URL.**~~ **Done.** The release workflow writes it
  from the artifacts it just signed and ships it as a release asset. Because GitHub does
  not serve assets of a *draft* release, publishing the draft is what makes an update
  visible — a deliberate gate. **Still untested end to end**, because that needs a real
  tag pushed through CI; the manifest shape is covered by a test against real artifacts.
- ~~**Move the takeover sequence into a library crate and test it.**~~ **Done.** Every
  judgement is now a pure function in `of_contention::plan`, tested — including the guard
  that was once shipped missing. The command-line example was **deleted** rather than
  updated: it kept a hand-rolled copy of that guard, and two implementations of a
  safety-critical flow with only one of them tested is precisely the drift that caused the
  original incident. The service is the implementation; `--diagnose` and the pipe cover
  the diagnostic need.
- ~~**Ask FanControl to stand down rather than killing it.**~~ **Done.** It documents
  `-e --exit`, which works in about four seconds where window messages did not, and it
  restores firmware control on the way out. `of_contention` now tries an application's own
  documented exit command before anything else. See the Findings entry.
- **Contention with other fan control software (high priority, user-requested).** Detect
  that another application is driving the same headers, surface it plainly in the UI, and
  offer to shut it down *reliably*. Register-level coexistence via the ISA bus mutex is
  necessary but nowhere near sufficient — it stops corrupted reads, not two controllers
  fighting over the same PWM channel. Needs: identifying the competing process, detecting
  a duty we did not command. *(The `can_restore_firmware_control()` clause is obsolete:
  releasing is one-way and always ends at the board's fan curve, so there is no contended
  case it cannot handle.)* Detection and takeover are **built and verified**; what remains
  is detecting a duty we did not command, which needs a contested-write probe.
  See the Findings entry for the reference machine.
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
2. ~~**Should the control engine eventually run as a Windows service?**~~ **Answered:
   yes, and it does.** Implemented 2026-09-22 — see the Findings entry. Kept below for the
   reasoning that led there.

   **Should the control engine eventually run as a Windows service?** A service would
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
- **2026-09-21** — Phase 3 steps 0 and 2, on the reference machine `Quasar` (ASUS ROG
  STRIX X570-I GAMING, Nuvoton NCT6798D, Ryzen 9 5950X). First time any of this project
  has run against real silicon. Read-only throughout: no fan, PWM or configuration
  register was written, and the backend refuses control by design until the restore path
  is proven with the user present. Outcomes:
  - The Phase 1 FFI works — but nothing could reach it. `PawnIOLib.dll` is installed
    somewhere on no search path, so `is_available()` answered **false on a machine where
    PawnIO was installed and running**. A backend trusting it would have silently used the
    mock on a machine with real fans. Now resolved via the driver service's `ImagePath`.
  - **PawnIO requires elevation.** Every call from an unprivileged process fails
    `E_ACCESSDENIED`. Direct evidence for Open Question 2.
  - PawnIO ships **no hardware modules**; `LpcIO` is a separate download. Missing module
    is now a distinct first-run state from missing driver.
  - **The ISA bus mutex is mandatory**, not advisory, and its scope was a real bug: held
    for the handle's lifetime it would have hung every other monitoring tool for as long
    as OpenFan ran. It now covers a transaction, enforced by the type system.
  - Decode verified against values observed in another application at capture time, and
    pinned by a register dump committed as a fixture. The hardware corrected a plausible
    assumption: the seventh tachometer is at `0x4CE`, and the `0x4CC` an even stride
    predicts returns garbage that would have read as a healthy 65311 RPM fan.
  - A batched read of every sensor costs **~0.5 ms**, 0.5 % of a 10 Hz tick.
  - Left undone on purpose: Step 3 (control). It needs the user present, and the headers
    are currently owned by another controller — so what `acquire()` would capture is that
    tool's manual mode, not the firmware's. 133 tests.
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
- **2026-09-22** — **Phase 3 Step 3 complete: PWM control works and hands back.** All five
  items of the control sequence verified on `Quasar`'s NCT6798D, in the required order,
  with Cameron present. Every write was on **channel 0 — the chassis header, which has
  nothing plugged into it** and was the only channel still under firmware control, so a
  mistake could neither stall a fan nor stop any cooling.

  ```text
  before:    mode=SmartFanIv  duty=71.8 %
  recorded:  mode=0x40 (SmartFanIv)  duty=181
  held:      mode=0x00 (Manual)      duty=181   <- taking it changed no speed
  commanded: 242   read back: 242 (94.9 %)
  held 3s:   242                                <- firmware is not overriding us
  fan/0:     0 RPM                              <- nothing connected, as expected
  after:     mode=0x40  duty=181                <- both bytes restored exactly
  then:      duty moved 181 -> 178 with nobody writing it
  ```

  Reproduced four times; the acquire/release half ran three times standalone first. The
  final register state was confirmed independently from a fresh dump each time, not taken
  from the program's own report.

  The last line is the one that matters. Restoring a mode byte only proves a byte was
  written — the chip *resuming control and moving the duty itself* is what proves control
  went back. That is now a checked outcome of the test, not an inference.

  Things this established, beyond "it works":
  - **The PWM write registers are not the readback registers.** `0x109/0x209/0x309/0x809/
    0x909/0xA09/0xB09` versus `0x01/0x03/0x11/0x13/0x15/0x17/0x19`. Recall had these
    conflated. A duty written to a readback address does nothing at all: the firmware keeps
    choosing the speed while we believe we are driving the fan. Separate tables, with a
    test asserting they differ.
  - **The fan mode register's low nibble is the firmware's tolerance setting** and must be
    preserved when switching to manual, or `release` is a lossy restore.
  - **Two orderings are load-bearing.** `acquire` writes the duty it just read *before*
    switching to manual, or the fan jumps to whatever was last in the manual register.
    `release` restores duty *before* mode, so the firmware algorithm never runs for an
    instant against a duty we chose. Verified: `held` duty equalled `recorded` duty on
    every run.
  - **A firmware-controlled duty drifts continuously**, which broke the first version of
    the test. Comparing a duty read before a confirmation prompt with one read after
    measures how long the operator took, not whether the restore worked — the error was 13
    raw counts when the prompt sat unanswered and 2 when answered quickly. Verification is
    now against the bytes `acquire` recorded. **Any future check involving a
    firmware-controlled duty must not assume it is stable.**

  Left deliberately undone, and why:
  - **No fan has actually been driven yet.** Channel 0 is empty. Proving a *fan responds*
    means channel 1, the only fan cooling this CPU, and that needs the other controller
    stopped and a reboot first — see below.
  - **`min_reliable_duty` is still `None`.** The stall point cannot be measured without a
    fan, and it is the one experiment where going quiet is the goal.
  - **Control stays opt-in.** `enable_control()` is called only by the `control-test`
    example; the application does not call it, so the app remains read-only.

- **2026-09-22** — **Releasing is one-way: it ends at the board's fan curve, always.**
  Decided by Cameron, and it retires the per-channel dilemma below rather than solving it.
  Switching control over from another application does not oblige us to reinstate that
  application's settings — and reinstating them would hand back a duty frozen at one
  number with nothing responding to temperature, which is the state this project exists to
  prevent. So `release` is defined by where it *ends*, not by what it found: a channel
  taken from the firmware goes back byte for byte, and one found in another program's
  manual mode gets the firmware mode imposed instead.

  Consequence worth noting: `can_restore_firmware_control()` can now answer **`true`**
  unconditionally, because no case remains that it cannot handle. That is not cosmetic.
  The engine's default failsafe is `RestoreFirmware` and it downgrades to a fixed 100 %
  when a backend says it cannot restore — so answering honestly is what lets the quiet,
  correct outcome happen instead of the loud fallback. The decision itself lives in
  `nct6775::release_mode`, pure and tested over all 256 register values against four
  firmware modes.

- **2026-09-22** — ~~**`can_restore_firmware_control()` is per-backend but the truth is
  per-channel.**~~ *Superseded by the one-way release policy above; kept for the
  reasoning.* Now a concrete finding rather than a worry, because both cases exist on
  `Quasar` simultaneously:

  | | mode when found | releasing it means |
  | --- | --- | --- |
  | channel 0 | Smart Fan IV | genuinely handing control back to firmware |
  | channels 1, 4 | Manual (another controller put them there) | reinstating a fixed duty with **no thermal response** |

  `acquire` already records which. The trait signature cannot express it, so the backend
  answers for its weakest channel — `false` — and the engine failsafes to a fixed duty even
  where releasing would have been better. Never wrong, sometimes louder than necessary.

  **Recommendation for Phase 4:** change the trait to
  `can_restore_firmware_control(&self, channel: &ChannelId) -> bool`. This touches
  `of-hal`, `of-hal-mock` and `of-engine`'s policy tests, and it makes Open Question 4's
  answer — "restore firmware control where possible, 100 % where not" — expressible per
  channel, which is the granularity it was always about.

- **2026-09-22** — Gap confirmed by accident, worth keeping. The first `control-test` run
  aborted at a confirmation prompt, and an early version would have returned there
  *without releasing*, leaving the channel in manual with nobody driving it. Fixed by
  releasing unconditionally. **The same gap exists in the engine**: a hard kill between
  `acquire` and `release` strands the channel. That is supervisor layer 5, the external
  watchdog, and today is evidence it is not a hypothetical.
