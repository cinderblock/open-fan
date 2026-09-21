# Phase 3 — Hardware bring-up on the target machine

> **You are running on the reference machine.** That is the whole point of this phase:
> everything up to now was built and tested against a simulated thermal plant, because the
> development box is an Intel NUC with no PWM headers. You have real silicon. Nobody else
> has been able to run a single line of this against real hardware yet.
>
> Read [`open-fan.md`](open-fan.md) first — it is the living plan and holds the decisions,
> the licensing analysis and the traps already identified. This document is the specific
> brief for Phase 3. Keep **both** current as you go.

## The single most important thing

**You are writing code that controls the only thing preventing thermal damage to a machine
Cameron owns and uses.** Not a test rig. Treat every PWM write as consequential.

Specific consequences of that, which are not negotiable:

- **Do not write a PWM register until you have read it, recorded it, and proven you can
  restore it.** Read/restore comes before write, always. The order is not an implementation
  detail — it is the difference between a recoverable mistake and a machine that needs a
  power cycle to cool down.
- **Have Cameron present for the first writes on each new chip or header.** They need to
  hear the fans and be able to hard-power-off. Ask before the first write session; do not
  schedule it for yourself.
- **Never set a duty below the stall threshold while exploring.** A stalled fan reads as
  "quiet" and is in fact "not cooling". When in doubt, go loud.
- **Never disable the firmware's own fan control without a tested way to give it back.**

If any of that conflicts with making progress, stop and ask. Slow is fine here.

## What already exists (do not rebuild it)

Phases 1–2 are done, on `master`. 76 tests, `cargo fmt`/`clippy -D warnings` clean.

| Crate | What it gives you |
| --- | --- |
| `of-units` | `Quantity`, `Value`, the connection rule. Values saturate into physical limits. |
| `of-core` | Graph model, validation, 16-node catalogue, evaluator. No I/O, no clock — `dt` is passed in. |
| `of-hal` | **The traits you are implementing.** `SensorSource`, `OutputChannel`, `Backend`, `Discovery`. |
| `of-hal-mock` | Simulated thermal plant with transport delay. Keep it working; it is what makes CI meaningful. |
| `of-hal-pawnio` | **PawnIO FFI, already written and compiling.** `PawnIo::load_module`, `execute`, `is_available`, `library_version`. Untested against a real driver — you are the first. |
| `of-engine` | `policy` (failsafe rules), `engine` (ticks when told, reads no clock), `runner` (the only threaded/clocked part). |
| `src-tauri` | Tray, single-instance, autostart, close-to-tray. A `hardware_status` command already calls `of_hal_pawnio::library_version()`. |
| `ui` | React Flow editor enforcing the type system. Not yet wired to the backend — that is the *other* half of Phase 2 and is **not your job**. |

### Invariants the existing code holds. Do not break them.

1. **Faults propagate; defaults are never substituted.** A sensor that cannot be read is
   *omitted* from `read_all`'s map — not given a plausible number. The graph turns that into
   NaN, sinks refuse it, and the channel failsafes. There are tests pinning this in
   `of-core/src/tests.rs` and `of-engine/src/policy.rs`. **If one of those tests is in your
   way, your change is wrong, not the test.**
2. **Every acquired channel is written every tick** — commanded or failsafed. Acquiring a
   channel registers a policy entry precisely so it can never be skipped.
3. **`acquire` records enough state for `release` to restore firmware control.** If your chip
   cannot do that, say so via `can_restore_firmware_control() -> false`; the engine then
   downgrades the failsafe to 100 % instead of releasing into an unmanaged manual mode.
4. **`read_all` is batched.** One bus transaction per tick, not one per sensor.
5. **`release` is on the dying-breath path**: no allocation, no locks, no async.
6. **No WinRing0, ever.** `deny.toml` enforces it. PawnIO only.
7. **Clean-room.** Datasheets, Linux `hwmon` source (read for *facts*, do not copy code into
   an MIT file), your own experimentation. Never decompile anything.

## Your goal

Make `of-hal-pawnio` a real `Backend`: enumerate this machine's sensors and fan headers, read
them, and control them safely — then prove it with the app actually running.

### Step 0 — Survey before you write anything

Record findings in `plans/open-fan.md` under Findings as you go. A future session should not
have to re-derive any of this.

- Motherboard model, BIOS version, Super I/O / EC chip (`Get-CimInstance Win32_BaseBoard`;
  HWiNFO64 names the chip directly).
- **The header map**: which physical header is which channel, what is plugged into each, and
  which tachometer belongs to which header. Get this from Cameron — it cannot be inferred
  reliably and guessing it is how the wrong fan gets stopped.
- Is PawnIO installed? `of_hal_pawnio::library_version()` tells you. If not, that is the
  first thing to sort out — see below.
- What does the firmware do when we are *not* running? Note the BIOS fan settings. This
  feeds the "what happens when OpenFan is dead" documentation item in the backlog.

### Step 1 — PawnIO

PawnIO is a **user-installed prerequisite**, from <https://pawnio.eu>. Ask Cameron before
installing it; it is a kernel driver and the ops rules in the global instructions apply.

Key facts already established (do not re-research):

- The C ABI is `pawnio_open` / `pawnio_load(blob)` / `pawnio_execute(name, u64[] in, u64[] out)` /
  `pawnio_close`, all returning `HRESULT`. Bindings are written in `crates/of-hal-pawnio/src/ffi.rs`.
- Hardware logic lives in **signed modules** from
  <https://github.com/namazso/PawnIO.Modules> releases. `LpcIO` is the Super I/O one.
  Also available: `LpcACPIEC`, `IsaBridgeEC`, `IntelMSR`, `AMDFamily17`, `RyzenSMU`,
  `SmbusI801`, `SmbusNCT6793`, `IntelPCHThermal`, `Nvidia`.
- **The signed edition enforces module signatures.** You cannot ship a custom Pawn module.
  If this board needs something not upstream, the path is *upstream a module to
  PawnIO.Modules*, not ship a blob. Note the gap in the plan and move on.
- **Do not vendor PawnIO source.** Dynamic linking is what keeps OpenFan MIT.
- Module blobs are not ours to redistribute — load them from the PawnIO installation
  directory at runtime, and handle "not found" as a first-run condition with guidance,
  not a crash.

`PawnIoError::ModuleRejected` already carries the signed-edition explanation. Make failures
here *explain themselves*; "HRESULT 0x80070005" helps nobody.

### Step 2 — Discovery and reading (safe: no writes)

Implement `SensorSource` for a real backend. This whole step is read-only, so it is the
right place to spend time and build confidence.

- Probe the Super I/O via `LpcIO`, identify the chip, enumerate temperatures, fan
  tachometers, voltages.
- Sensor ids must be **stable across reboots and re-enumeration** — something like
  `nct6687/temp/cpu`, not an array index. Profiles reference these ids; an id that moves
  silently re-points a user's fan curve at a different sensor.
- Map each to the right `SensorKind`. The engine converts that to a `Quantity`, and a
  reading arriving as the wrong quantity faults rather than being used — so getting this
  mapping right *is* a safety property, not cosmetics.
- Sanity-check against HWiNFO64 side by side. Decoding registers wrong is easy and a
  plausible-but-wrong temperature is the worst possible output.
- An unreadable sensor is omitted from the map. Do not invent a value.

### Step 3 — Control (the dangerous part)

Implement `OutputChannel`. In this order, and not a different one:

1. `channels()` — enumerate headers. Populate `tachometer` where a tach maps to a header,
   and `min_reliable_duty` once you know the stall point.
2. `acquire()` — **read and store the existing PWM mode and value.** This is what makes
   everything after it recoverable. Idempotent.
3. `release()` — restore exactly what `acquire` recorded. **Test this before you ever
   write a duty**: acquire, release, confirm via HWiNFO that the firmware curve is back.
4. `can_restore_firmware_control()` — honest answer. `false` is a perfectly good answer and
   the engine handles it correctly.
5. `set_duty()` — only now. First write should be a *higher* duty than current, on a header
   Cameron has agreed to, with them watching.

Finding the stall point means ramping **down** slowly while watching the tachometer — the
one experiment where going quiet is the point. Do it deliberately, with the machine idle,
and record the result per header.

### Step 4 — Prove it end to end

- `Discovery::discover()` returning the real backend, `Ok(None)` when the hardware or driver
  simply is not there (that is an ordinary outcome, not an error).
- Wire it into `src-tauri` so the app selects the real backend when available and falls back
  to `of-hal-mock`. Keep the mock reachable — ideally behind a flag — because it is what lets
  this project be developed on machines with no fans.
- Run the actual app (`bun run tauri dev`), load a simple curve graph, and watch a real fan
  respond. Then close the window and confirm control continues. Then quit from the tray and
  confirm the header goes back to firmware.
- Screenshot or describe what you saw. Do not report success from tests alone in this phase —
  tests cannot hear a fan.

## Testing expectations

- Hardware-touching code cannot run in CI, so it must be **thin**: register decode and
  bit-twiddling belong in pure functions taking bytes and returning values, tested with
  captured register dumps. Put the dumps in the repo as fixtures — they are also the
  evidence for anyone adding a second chip later.
- Keep every existing test green. `cargo test --workspace` must pass on a machine with no
  PawnIO installed, so gate real-hardware tests behind `#[ignore]` or a feature.
- The existing `of-hal-pawnio` tests assert that a *missing* driver produces a clean error
  rather than a panic. Keep that property — most users' first run will be exactly that.

## Working agreement

- Commit at logical steps; run `cargo fmt --all`, `cargo clippy --workspace --all-features
  --all-targets -- -D warnings`, `cargo test --workspace` first. See `CLAUDE.md`.
- Heavy builds go through the compute broker:
  `node ~/.claude/bin/cpu-slots.mjs run --slots 4 --label "open-fan build" -- cargo build`.
  A cold Tauri build is ~10 minutes.
- **Ask before**: installing PawnIO, the first PWM write on any header, and anything that
  changes BIOS/firmware settings.
- Update `plans/open-fan.md` Findings in the same turn you learn something — especially
  negative results, with the error text that proved it.

## Open questions you may be able to answer

These are in `open-fan.md` and Phase 3 is where evidence for them appears:

1. **Can we read, or even write, the board's firmware fan settings?** If the BIOS curve is
   readable, the "what happens to my fans when OpenFan is not running" explanation becomes
   real rather than generic. Likely vendor-specific; a negative result is still valuable.
2. **Should the engine eventually run as a Windows service?** Whatever you learn about
   whether PawnIO works from session 0 bears directly on this.
3. **What should the default failsafe be per channel?** Current recommendation: restore
   firmware control where possible, 100 % where not. If a header turns out not to restore
   cleanly on this board, that is exactly the evidence needed.

## Things not to do

- Do not use WinRing0 or any fork of it, under any circumstances.
- Do not decompile anything.
- Do not weaken a safety test to make hardware code fit.
- Do not write a PWM register you have not first read and proven you can restore.
- Do not explore duties below the stall point casually.
- Do not do the Phase 2 UI wiring — it is unfinished on purpose and belongs to a session
  that does not have hardware to risk.
- Do not bypass the `block-force-push` hook, and do not force-push.
