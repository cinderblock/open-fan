# In-chip control as a first-class choice

**Status: reading is built; writing is not.** `of_core::offload` decides whether a channel
could run in the chip and explains what stopped it. `of_hal_pawnio::nct6775` now *reads* a
channel's SmartFan IV curve — the registers are mapped and tested against a real capture.
Nothing writes a curve register yet; that is the step that moves fans and waits for a
person.

## The goal, stated carefully

Not "prefer our engine". The opposite: **make the chip's own abilities a real, obvious,
supported choice**, and make the boundary legible — so somebody can tell, without
guessing, which of the things they want needs software running and which does not.

Most people who install a fan controller want something the chip could already have done.
Making them run a background service for it is a worse product, not a better one. The
engine earns its place only where the chip genuinely cannot go, and a user deserves to
know which side of that line their configuration sits on *before* they commit to it.

## Why the chip is worth choosing

| | In-chip | Userland engine |
| --- | --- | --- |
| Needs software running | no | yes |
| Works before sign-in | yes | yes (the service starts at boot) |
| Survives our crash | **yes** | no — falls to the failsafe |
| Survives sleep/resume | probably; unverified | yes |
| CPU cost | none | a tick loop |
| Sensors it can read | only what is wired to it | anything the OS can see |
| Logic | one temperature, a few points | the whole graph |

The row that matters most is "survives our crash". Fan control that keeps working when the
controlling software dies is a better safety story than any failsafe we can write, and it
is the honest reason to offer this rather than a performance argument.

## What the chip can actually be given

SmartFan IV, per channel: a temperature source, a handful of temperature/duty points,
step-up and step-down times, a tolerance, a critical temperature, and start/stop duty.
The other modes (Thermal Cruise, Speed Cruise) target a temperature or an RPM instead.

That is the entire vocabulary. There is no instruction set and nowhere to put a graph.

## What can never move into the chip

This list is the product feature. It is what the interface has to be able to *explain*.

- **Sensors the chip cannot see.** GPU temperature does not exist from its point of view,
  nor do drive temperatures, nor anything computed in software. This is permanent and is
  the most common reason a configuration cannot be offloaded.
- **More than one temperature per channel.** "Whichever is hotter, CPU or GPU" has nowhere
  to live.
- **Everything stateful we model**: PID, rate limits, low-pass filtering, hold bands,
  comparators and select, delay, tachometer feedback into logic.
- **Fixed RPM on a channel**, unless the chip's Speed Cruise mode is used — which is a
  different mode with different registers, not a curve.

## Reading the curve: done, and a caution about how

Decoded and tested in `of_hal_pawnio::nct6775` (`decode_smart_fan_curve`,
`read_smart_fan_curve`). Addresses come from the Linux `nct6775` driver and are checked
against a 16-bank capture from this board (`tests/fixtures/nct6798d-quasar-full.txt`).

**The addresses that shipped are not the ones first guessed.** An earlier pass "found" the
curve at `0x?11`/`0x?17` by shape — a perfectly plausible monotonic ladder sits there. It
is the wrong table. The driver puts SmartFan IV at `0x?21` (temperatures) and `0x?27`
(duties), and re-reading the same dump there gives a *different*, equally plausible ladder.
The lesson is banked: a convincing shape is not an address, which is why nothing here
relies on inference. What the `0x?11`/`0x?17` ladder actually is remains unmapped.

Decoded from the full capture, all seven channels read as monotonic curves. The two the
BIOS treats as CPU fans follow temperature **source 28 — "PECI Agent 0 Calibration"** —
not a thermistor pin. On this AMD board that is how the firmware routes CPU temperature,
and it is a source `TEMP_INPUTS` does not expose (it has SYSTIN and CPUTIN only). Offloading
a CPU curve cannot bind the same reading the BIOS uses until that source is exposed. There
is a test pinning this so it is not forgotten.

## Whose curve is it? The registers do not say

Channel 1 above is in **Manual** — software had taken it when the dump was captured — and
its curve registers still hold a coherent ladder. So whatever took that channel moved the
mode nibble and left the curve alone. That matches the live observation that FanControl's
documented exit restored channels to SmartFan IV, which it could only do by preserving it.

But **a register carries no provenance.** We read a value; we cannot read who wrote it.
So two different questions have two different answers:

- *Is this a plausible fan curve?* — yes, and we can read it.
- *Is this the **BIOS's** curve?* — **not knowable from the registers alone.**

"Looks untouched" is not proof of untouched, and vendor software with its own kernel driver
can write things we never observe.

The only reliable baseline is to **read the whole per-channel configuration at service
start, before enabling control and before anything else has had a chance to run**. The
service starts at boot, which makes it the one component positioned to capture that. It
should snapshot then, and treat any later reading as "current state" rather than as the
board's intent.

This is the same discipline already applied to the mode nibble — read it, record it, prove
you can restore it — widened to the register set that describes a curve.

## Why we do not read the temperature the BIOS curves against

The chip has two namespaces that are easy to conflate:

- **Sources** — a 5-bit index naming a physical or bus-fed temperature: thermistor pins
  (SYSTIN, CPUTIN, AUXTIN0-4), CPU-reported temperature (the "PECI Agent" entries; on this
  AMD board that path carries the CPU's own reading), SMBus and virtual sources.
- **Monitored slots** — six value registers software can read (`0x027`, `0x150`, and four
  more), each with a source-select register (`0x621..0x626`) saying which source it shows.

Software reads *slots*. The fan-control block follows *sources* directly. Those are
different paths, and the dump shows the consequence:

| slot | select | shows | value |
| --- | --- | --- | --- |
| 0 (`0x027`) | 1 | SYSTIN | 44 °C |
| 1 (`0x150`) | 2 | CPUTIN | 43 °C |
| 2-3 | 0 | nothing | 0xFF |
| 4-5 | 31 | Virtual | 0xFF |

The BIOS points the CPU fan curves at **source 28** but never pointed a slot at it. The
chip is *using* that temperature without *publishing* it. There is no register we can read
to see the number the fans follow.

Two conclusions, one uncomfortable:

1. **Our exposed inputs are not fixed-function.** `TEMP_INPUTS` calls `0x027`/`0x150`
   SYSTIN/CPUTIN and the code comment claimed they were thermistor pins. They are slots
   0 and 1, correct today only because the BIOS selected sources 1 and 2. The very risk the
   comment warned about — a stable id whose meaning a BIOS update can change — applies to
   what we already ship. The fix: key ids by source, check the select at read time, and
   refuse the reading if the slot has been re-pointed. Not done yet; a fixture test now
   pins the current selects so the assumption is at least checked against real bytes.
2. **Reading source 28 requires a configuration write** — pointing an idle slot (2 or 3)
   at it and reading that slot's value register. Small, reversible under record-before-
   write, on a slot nothing uses. But it is a write to BIOS-set configuration, and it is
   the first one of its kind, so it waits for a person. Which value register the idle
   slots use on the 6798 needs the driver's NCT6779 monitor tables, not the NCT6775 ones
   quoted above.

Until (2) is done, `offload` can produce a CPUTIN curve that the chip would follow a
*different* temperature than the firmware does. That is a correctness gap, not a missing
feature, and it is why exposing the CPU source is a prerequisite rather than a nicety.

## Persistence: the chip cannot keep it, and that is fine

Those registers are volatile RAM. There is no user-writable non-volatile store for fan
configuration on this family.

But the thing that erases a setting is not the chip forgetting — it is **BIOS deliberately
reprogramming the channel at every boot** from its own NVRAM. Making a curve survive that
would mean writing a board vendor's private UEFI variables: undocumented, vendor-specific,
and the class of write that bricks boards. Out of scope, permanently. The only legitimate
way to make a curve persist is for a person to enter it in BIOS setup.

**Re-application replaces persistence.** The service already starts at boot, so:

```
power on   → BIOS programs its own curve; it runs immediately and is safe
service up → writes the user's curve into the chip
           → steps out of the control loop entirely
```

Set once by the user, run by the chip. The service is then not a participant, which is the
whole point: it can crash, be killed, or be updated and the fans still follow *the user's*
curve rather than the board vendor's.

Two things to verify rather than assume:

- The window between power-on and the service starting. BIOS's curve covers it, so this is
  safe, but it is not silent — a machine may be briefly louder or quieter than configured.
- **Whether S3/S4 resume resets the chip.** If it does, re-apply on resume. Unverified.

## The interface question

A channel is in one of three states, and the interface should name them plainly:

1. **Board default** — the BIOS curve, untouched. What a fresh install finds.
2. **In chip** — the user's curve, programmed into the chip, running without us.
3. **OpenFan engine** — the graph drives it every tick.

The move that makes this legible: when a user builds something in the editor, tell them
which side of the line it lands on, and *why*. A subgraph that reduces to
`{one on-chip temperature → piecewise curve → output}` can be offloaded; anything else
names the specific reason it cannot — "this follows your GPU temperature, which the
motherboard chip cannot read".

That diagnosis is a pure function over the graph and belongs in `of-core` or beside it,
testable without hardware. It is the piece worth building first, because it is what makes
the choice clear even before any offload exists.

## What the diagnosis says about a real configuration

`of_core::offload::reduce_all` run over the graph our own FanControl importer produces
from the reference machine's configuration:

```
nct6798d/pwm/1: stays in software — The "Low-pass filter" step has no equivalent in the
                motherboard chip. The chip has its own step-up and step-down timing, which
                is similar but not the same — removing this step would let this fan run in
                the chip.
nct6798d/pwm/4: could run in the chip — Fixed { duty: 23.4 }
```

That is the shape the feature should have: one fan is told exactly what to give up and
what it would gain, and the other is told it does not need us at all.

Some nodes fold rather than block. A `Clamp` after a curve is the same as clamping its
points, and `Offset` and `Scale` shift and stretch them — which matters because a
minimum-duty floor from an import arrives as exactly that, and refusing to offload over it
would be a needless "no".

### A fixed duty in the chip is not the same kind of safe as a curve

`pwm/4` above reduces to a constant, and a constant in the chip means Manual mode with a
duty left in the register. It genuinely runs without us — but it **has no thermal
response**. If the machine heats up, nothing moves.

That is the same register state as a *stranded* channel, which this project treats as a
fault. The difference is entirely intent, and intent is not readable from the chip. So
offloading a fixed duty must be a louder decision than offloading a curve: it deserves its
own confirmation saying the fan will not respond to temperature, and the channel should
not afterwards be reported as stranded by our own contention survey.

## What would have to be built

1. ~~**The pure diagnosis.**~~ **Done** — `of_core::offload::reduce` / `reduce_all`, with
   folding for `Clamp`/`Offset`/`Scale` and an `explain()` per obstacle. No I/O, tested
   without hardware, including against the graph the importer really produces.
2. ~~**Register mapping for SmartFan IV (read).**~~ **Done** — `REG_AUTO_TEMP`/`REG_AUTO_PWM`,
   `REG_TEMP_SEL`, step times, tolerance, critical temperature, start/stop, all decoded and
   tested against a real capture. The *write* path is not built.
3. **Record-before-write, extended.** The existing discipline — read it, record it, prove
   you can restore it — now covers a dozen registers per channel instead of two. The
   recording burden grows with the register count and must not be skipped.
4. **Re-apply at boot, and on resume if resume proves to reset the chip.**
5. **Expose the monitored temperature sources**, at least the PECI one the BIOS uses for
   CPU fans on this board. Without it, `offload` can produce a curve on CPUTIN that the
   chip cannot follow the same way the firmware does — a correctness gap, not just a
   missing feature.
6. **Interface**: the three states above, per channel, with the reason shown when a
   configuration cannot be offloaded.

## Risks

- **Overwriting the BIOS's curve in RAM.** Once we write auto-points, "hand back to
  firmware" no longer restores the board's curve unless we recorded and restore those
  registers too. Today handing back is cheap because we only moved a nibble.
- **A wrong curve is a real thermal risk**, and unlike a wrong duty it persists after we
  exit — that is the point of the feature and also its danger. The stall floor matters
  here: the calibration import already records where a fan stops.
- **Nothing survives a power cycle**, which is the compensating safety property. A bad
  curve cannot brick anything; BIOS reinstates its own at the next boot.
