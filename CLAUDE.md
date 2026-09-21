# OpenFan — notes for AI sessions

**Read `plans/open-fan.md` first.** It is the living plan: current phase, decisions
already settled, research findings, and the traps this project has already identified.
Keep it current in the same turn you learn something, especially negative results.

## Hard rules

- **Never use WinRing0**, or any fork or wrapper of it. It is unmaintained, allows
  arbitrary physical memory access from an unprivileged caller, and Defender flags it
  under CVE-2020-14979. `deny.toml` enforces this; do not weaken it. PawnIO is the
  supported path.
- **Clean-room.** Never decompile or disassemble a closed-source hardware tool, and never
  read or copy decompiled output. Hardware facts come from datasheets, open drivers and
  experimentation. Parsing another tool's documented-by-observation config file for
  *import* is fine and is the only place a competitor is referenced.
- **Do not vendor PawnIO source.** We link `PawnIOLib.dll` dynamically at runtime. That is
  what keeps an LGPL/GPL driver ecosystem compatible with an MIT binary.
- **The UI is never in the control loop.** If a decision needs the webview alive, it is
  wrong. Closing the window hides it; the engine keeps ticking.
- **Never substitute a default for a failed sensor reading.** Faults propagate; sinks fail
  safe. This is load-bearing, and it is pinned by tests in `of-core` and `of-engine` —
  if one of those tests is in your way, the change is wrong, not the test.
- **Nothing on the dying-breath path may allocate, lock or await.**

## Where things live

`of-units` owns the connection rule — the single definition both the backend validator and
the editor check route through. `of-core` is pure: no I/O, no async, no clock, so the
control behaviour is testable without hardware or timing. `of-engine` owns the failsafe
policy. `src-tauri` and `ui` own presentation only.

`ui/src/bindings/` is generated from Rust by `bun run bindings` and **is committed**. Run
it after changing a shared type or CI fails.

## Checks before committing

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bun run build
```

Heavy builds go through the compute broker on this machine:
`node ~/.claude/bin/cpu-slots.mjs run --slots 4 --label "open-fan build" -- cargo build`.

## Reference platform

The dev box (`Noook`) is an Intel NUC with no PWM headers — build and test here against
`of-hal-mock`, but never treat it as a control testbed. The real target machine is a
separate desktop; see the plan's open questions.
