# OpenFan

Fan control built on a node graph whose connections are **typed by physical quantity**.

A temperature output cannot be plugged into a PWM duty input. If you want that
conversion you add a curve node and say what it means. The result is a fan configuration
that is self-documenting, statically checkable, and composable — and one where a whole
category of "why is my machine loud / why is it cooking" misconfigurations simply cannot
be expressed.

> **Status: early.** The graph core, the hardware abstraction and the safety engine exist
> and are tested; the editor renders and enforces the type system. Real hardware control
> is not wired up yet. Do not rely on this to cool a machine you care about.

## Why typed connections

Most fan controllers give you a list of curves and a set of dropdowns. That works until
you want something slightly unusual — mix three sensors, hold a floor while a drive is
busy, ramp differently on the way down than on the way up — and then you are fighting the
UI's imagination rather than describing what you want.

A dataflow graph describes all of it directly. Types are what keep that from becoming a
footgun:

- `Load` and `Duty` are both percentages. They do **not** interchange. "The CPU is 70 %
  busy" and "drive this fan at 70 %" are different claims, and silently equating them is
  the exact mistake the type system exists to prevent.
- Every conversion is a visible node, so reading the graph tells you what the machine
  will actually do.
- The rule is defined once, in Rust, and the editor's connection check and the backend's
  validator both route through it. They cannot disagree.

## Safety

Fans are the only thing between silicon and thermal damage, so the engine is built around
the assumption that our own software will eventually misbehave.

- **Sensor faults propagate; they are never substituted.** A dead sensor yields NaN, not a
  plausible-looking default. A mixer with one dead input poisons its output rather than
  averaging the failure away into a comfortable number. A clamp cannot launder a NaN into
  a confident duty.
- **Every channel is accounted for on every tick** — either commanded from the graph or
  explicitly put into its failsafe. A channel is never left silently unwritten, and never
  held at a stale duty from before its sensor died.
- **The failsafe hands control back to the firmware.** Your board's BIOS/EC has its own fan
  curve and will keep running whatever happens to us. Where a device cannot be handed
  back, the failsafe is full duty instead — loud, but alive.
- **Closing the window does not stop control.** The engine is the application; the window
  is a view of it. OpenFan minimises to the tray and keeps ticking.

Layered watchdogs, the dying-breath handler, crash reporting and auto-restart are
specified in [`plans/open-fan.md`](plans/open-fan.md) and land in Phase 4.

## Hardware access

OpenFan uses [PawnIO](https://pawnio.eu), a signed kernel driver that runs sandboxed
bytecode modules, for low-level sensor and PWM access on Windows. **PawnIO must be
installed separately**; OpenFan will start without it and run in a read-only degraded
mode.

OpenFan does **not** use `WinRing0`, the ring-0 shim most Windows hardware tools were
built on. It is unmaintained, allows arbitrary physical memory access from an
unprivileged caller, and Microsoft Defender now flags it under CVE-2020-14979 — using it
means asking every user to add antivirus exclusions or turn off Memory Integrity. That is
not an acceptable trade, and no dependency in this repository may reintroduce it; CI
enforces that via `deny.toml`.

## Building

Requires [Rust](https://rustup.rs) and [Bun](https://bun.sh).

```sh
bun install
bun run bindings     # regenerate ui/src/bindings from the Rust types
bun run tauri dev    # run the app
```

Checks:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bun run build        # typecheck + frontend bundle
```

`ui/src/bindings/` is generated from the Rust definitions in `of-units` and committed, so
the frontend builds without a Rust toolchain. CI fails if it is stale — run
`bun run bindings` after changing a shared type.

## Layout

| Path | What it is |
| --- | --- |
| `crates/of-units` | Physical quantities and the connection rule |
| `crates/of-core` | Graph model, validation and evaluation — no I/O, no async, no clock |
| `crates/of-hal` | Hardware traits |
| `crates/of-hal-mock` | Simulated thermal plant with real dynamics, for tests and CI |
| `crates/of-hal-pawnio` | PawnIO FFI (Windows) |
| `crates/of-engine` | Failsafe policy and the tick-to-hardware rule |
| `crates/of-config` | Versioned profile schema and importers |
| `crates/of-ipc` | Types shared with the UI |
| `src-tauri` | Desktop shell: tray, window lifecycle |
| `ui` | React + React Flow editor |

The graph core has no dependency on hardware and the engine has no dependency on the UI,
so the whole control path can be exercised in CI on a machine with no fans — against a
thermal model with transport delay that can genuinely oscillate, rather than a stub that
returns constants.

## Roadmap

See [`plans/open-fan.md`](plans/open-fan.md) for the full plan, the research behind the
architecture, and the open questions. In short: hardware support, then the safety
supervisor, then logging and charts, then signed auto-updating releases, then profile
import. Beyond that: servo control, limit-cycle detection with automatic damping, a case
visualizer, and Linux.

## Licence

MIT. See [LICENSE](LICENSE).

OpenFan is a clean-room implementation. Hardware knowledge comes from datasheets, open
drivers and experimentation — never from decompiling anything.
