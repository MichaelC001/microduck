# Moves as config, not code

Not a design yet — the current picture written down before there is one, so the thinking
behind PR2 is not lost and is not mistaken for a decision. See
[`policy-channel-design.md`](../design/policy-channel-design.md) for what shipped, and
[`robotd-design.md`](../design/robotd-design.md) §1.4 and §2.3 for the tick and the policy.

## Why

Adding a one-shot move today touches seven places: the `Skill` enum in `duck-ipc-proto`,
`Net` and `PolicyPaths` in `duck-control`, `Slot` plus a config key plus a registry entry in
`robotd-params`, a branch in `robotd/src/control.rs`, and padd's button table. A community
policy like `fffiloni/microduck-polite-bow-b1d864` — zero command, four seconds, selecting it
is the trigger — is the same shape as `roulade` and cannot be added without a release.

## What runs today, end to end

Three layers, and only the third is what PR2 touches. Keeping them apart is the point of
writing this down: the first two decide *whether* a policy drives, the third decides *which*.

### 1. padd — a client with three local modes

The mode is padd's own; `robotd` never hears about it. It changes what the sticks mean.

```text
                    Y                         B  (leaving snaps the body back)
        ┌────────────────────────┐   ┌────────────────────────────┐
        ▼                        │   ▼                            │
   ┌─────────┐               ┌───┴───────┐                  ┌─────┴──────┐
   │  Head   │◀────── Y ────▶│   Drive   │◀────── B ───────▶│  BodyPose  │
   └─────────┘               └───────────┘                  └────────────┘
   sticks →                  sticks →                       sticks →
   robot.head                robot.move                     robot.pose
```

Everything else is an edge or a hold, and goes straight out as an intent:

| control | sends |
| --- | --- |
| Start | `robot.enable {toggle}` — the robot owns the state, padd holds no belief |
| A | `robot.do GroundPick` |
| X | `robot.do Roulade` — held, it chains |
| LB / RB | `robot.do KickLeft` / `KickRight` |
| D-pad down | `robot.do SitToggle` |
| Select, held 2 s | `robot.shutdown` |
| D-pad up, held 3 s | `robot.setMode` (walk ↔ roller) |
| RT / LT edges | `robot.sound` — quack, wheee |

**The five skill names are compiled in**, as a fixed array of `(fired, Skill)` pairs. That
array is the thing PR2 replaces with a table.

### 2. robotd — two state machines that gate the policy

Neither knows anything about *which* policy runs. They decide whether one may.

```text
  bring-up                              limp-fall  ([safety] limp_fall)

  ┌──────┐  enable/init   ┌────────┐    ┌──────┐  predictor fires  ┌──────┐
  │ Limp │───────────────▶│ Homing │    │ Idle │──────────────────▶│ Limp │
  └──────┘                └────┬───┘    └──────┘                   └──┬───┘
      ▲                        │ ramp done  ▲                         │ gyro still
      │       relax            ▼            │                         ▼
      └──────────────────  ┌───────┐        │      ramp done   ┌────────────┐
                           │ Ready │        └──────────────────│   Posing   │
                           └───────┘                           └────────────┘

  driving = enabled ∧ Ready ∧ controller loaded ∧ ¬limp-fall
            ∧ sensors ∧ imu warm ∧ ¬powered-off
```

A policy change or a `robot.setMode` re-enters `Homing`, which is how a network is swapped
without the next tick being a different policy's idea of where the legs were.

### 3. The select cascade — one tick, first match wins

This is not a state machine. It is a priority list re-evaluated every tick, where each entry
owns a little state of its own (a countdown, a phase, a latch).

```text
  roulade?    ── yes ─▶  Net::Roulade      command [0,0,0]      window, chains while held
      │ no
  kick?       ── yes ─▶  Net::KickLeft     command [0,0,0]      window
      │ no                   /KickRight
  ground pick?── yes ─▶  Net::GroundPick   command [cos φ,sin φ,0]   phase over one period
      │ no
  sitting?    ── yes ─▶  Net::SitStand     command [1,0,0]      latched until untoggled
      │ no
  rising?     ── yes ─▶  Net::SitStand     command [0,0,0]      window, then falls through
      │ no
  ├─ will_stand(|twist|) or body pose active  ─▶  Net::Stand    the client's command
  └─ otherwise                                ─▶  Net::Walk     the client's command
```

**One-shots already run on top of walk and stand.** Nothing is disabled to fire a kick; the
cascade simply matches higher. The eviction a community policy needs today
(`policy load stand none`) is only because it goes into the `walk` slot — the fallback
itself — rather than into the cascade.

## What the first two arms have in common

```rust
} else if let Some((left, _)) = self.kick {
    let net = if left { Net::KickLeft } else { Net::KickRight };
    (net, Command::default(), label)
} else if self.roulade.is_some() {
    (Net::Roulade, Command::default(), "roulade")
}
```

The same arm twice. Kicks and roulade differ in four numbers — duration, action scale, gain
ratio, and whether holding chains another — and in nothing else. `polite-bow` is a fifth set
of the same four numbers.

So the first pass is: those two arms become one, driven by a list.

```toml
[[move]]
name = "roulade"
duration = 1.0
chain = true

[[move]]
name = "polite-bow"
path = "/var/lib/robot/policies/fffiloni/microduck-polite-bow-b1d864/main/policy.onnx"
duration = 4.0
```

`walk`, `stand` and `sitstand` stay as they are — the first two are the fallback pair and sit
is latched and driven internally by shutdown-sit and the seated-boot rise, not only by a
button. `ground_pick` stays too in the first pass: it is phase-scripted rather than
zero-command, and generalising it means the descriptor grows a command generator before
anything needs one.

## Open, and wanted before writing any of it

- **Per-move parameter overrides.** A move may need a different `limp_fall_tilt_z` or
  `cmd_alpha` while it runs, restored after — flamingo leans ~24° against a fall gate set at
  ~26°. This is the mechanism that keeps a move from needing global config changes a person
  has to remember to undo.
- **`Skill` becomes a string.** An open vocabulary on the wire, the five existing names
  reserved. `API_VERSION` bump, and every client that switches on the enum has to enumerate
  instead.
- **The fixed slots go.** A move carries its own path, so `policy.kick_left` stops being a
  config key and `robotctl policy load kick_left <file>` has no slot to name.
- **Where the binding lives.** `[pad] x = "polite-bow"` in `robotd.toml` means padd gains a
  config dependency it does not have today (it links `duck-ipc-proto`, `gilrs` and `clap`,
  and nothing else). One file keeps `robotctl configure` the single editor; the friction is
  that the registry is a flat key list and `[[move]]` is an array of tables.
- **Pending moves are an `AtomicU32` bitmask** in `robotd::intents`, so a move becomes an
  index. Fine to 32, worth knowing it is there.
