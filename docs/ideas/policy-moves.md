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

## Decided

- **`[[skill]]`, not `[[move]]`** — the wire already says `robot.do {skill}`, and `SkillTuning`
  and "one-shot skills" are the vocabulary in the code. One word, not two.
- **Absent means the built-in three.** A board that updates onto this keeps its kicks and its
  roulade with no config and no migration, and an entry merges by name — the file stays a list
  of decisions rather than a copy of the defaults, which is what `robotctl configure` already
  promises about every other key. `robotctl policy list` is where the resolved set is visible.
- **Per-skill overrides are raw parameter names** in a `[skill.params]` table — `cmd_alpha`,
  not an invented `smoothing = "off"`. Applied on entry, restored on exit. Bounded to a named
  set: tuning from `[policy]`, and only the handful of `[safety]` keys a move legitimately
  needs, because a skill that could widen a joint limit or the deadman would be reaching past
  the layer that makes a stranger's policy safe to try in the first place.
- **`Skill` becomes a string on the wire.** Nothing outside this workspace consumes the enum —
  `duckctl` does not touch it — so the `API_VERSION` bump has no client to coordinate with.
- **List order is priority**, replacing the hardcoded `roulade > kick` precedence.
- **`robotctl configure` support comes second.** Adding and removing skills is
  `robotctl policy`; the registry learning about repeating tables is the largest single piece
  of this and is orthogonal to whether skills work.

## Built

`[[policy.skill]]` and `robotctl policy add` / `remove` / `do <name>`. A community one-shot is
config: `robotctl policy add polite-bow fffiloni/microduck-polite-bow-b1d864` fetches it, takes
the length from its manifest and writes the entry.

One correction to the analysis above, from being told so. A policy whose manifest says
`kind: perpetual` is not thereby unusable as a one-shot — pressing a button, lifting a foot for
five seconds and putting it down is a one-shot interaction whatever the network's own lifetime
is. What perpetual means operationally is *who supplies the ending*: `polite-bow` is standing
again after its four seconds, so the window can simply expire, while flamingo holds until told
otherwise and a bare expiry would hand walk a robot on one leg. So a skill declares `command`
(the twist while it runs) and `unwind`/`unwind_s` (what it drives on the way back) — the daemon
supplying the ending the policy does not have, which is the two-phase shape the sit toggle
already uses on its way up.

**Adding a skill needs no restart.** `robot.reloadPolicies` re-reads `[policy]` from disk and
rebuilds at the home pose with torque on throughout — so a skill written by `robotctl policy add`
is one the robot has seconds later, without `systemctl restart robotd` dropping motor control and
putting the robot on the floor. The mode is carried over rather than adopted from the file,
because `robot.setMode` deliberately does not write config and a live switch must not be undone
by adding a bow. `[safety]` and `[control]` are still read once at startup.

**A running skill has no fall reflex.** The limp-fall predictor is only consulted while the
controller is not `busy()`, and any active skill makes it busy. That was uncontroversial when
every one-shot was under a second — a kick is over before a fall could develop — and it is worth
deciding rather than inheriting once a skill can be configured to hold for ten. It also means a
per-skill fall-gate override would be decoration, which is why there is not one.

## What a manifest has to say for a policy to be a skill

Most of it is already the published convention. This is what the daemon *acts on*, and what a
publisher has to get right for `robotctl policy add <name> <repo>` to work with no flags.

| field | needed for | what happens without it |
| --- | --- | --- |
| `obs_len`, `action_len`, `model_api`, `robot.model` | every policy | already read; a mismatch is refused before the download |
| `kind` | every skill | `"episodic"` or `"perpetual"` — the difference is who supplies the ending |
| `duration_s` | an episodic skill | `policy add` refuses and asks for `--hold` |
| `command.idle` | a perpetual skill | it unwinds to zeros, which for most policies is right and for some is not |
| `unwind_s` | a perpetual skill | it hands back the instant the hold ends, mid-pose |
| `action_scale` | optional | the gait's own scale is used |

**`kind` is about who ends it, not about how long it runs.** `polite-bow` is episodic: four
seconds later it is standing again, so the window can simply expire. Flamingo is perpetual: it
holds until told otherwise, and something has to tell it. Both are one-shots to a person pressing
a button.

**A perpetual policy cannot declare its own `duration_s`**, because there is not one — how long
to hold a foot up is the caller's choice. It declares how to *stop*, which is the part it knows.

### The three published today

- **`fffiloni/microduck-polite-bow-b1d864`** has no `manifest.json` at all. Its
  `preview_metrics.json` carries `duration_s: 4.0` and `observation_dim`/`action_dim`, so the
  information exists in a file nothing reads. It needs the manifest.
- **`RemiFabre/microduck-flamingo-cycle`** has everything but one field, and that one exists
  under a different roof: `eval.transition_time_s: 1.5` is the unwind, sitting in the block that
  records how it was evaluated rather than in the contract the daemon reads. Promoting it to a
  top-level `unwind_s` is the whole change; `command.idle` is already right.
- **`HannesVonEssen/microduck-running`** is a gait rather than a skill and needs nothing.

### The one still in code

`roulade` is 1.0 s and the kicks are 0.5 s because `builtin_skills()` says so. That is the same
coupling this exercise removed for *which file* a slot runs, still present for *how long* — a
retrained roulade of a different length needs a daemon release, or a config line overriding it by
name. Tolerable while the three are ours and change rarely, and worth revisiting the moment one
of them is retrained.

## Open, and wanted before writing any of it

- **Per-move parameter overrides.** A move may need a different `limp_fall_tilt_z` or
  `cmd_alpha` while it runs, restored after — flamingo leans ~24° against a fall gate set at
  ~26°. This is the mechanism that keeps a move from needing global config changes a person
  has to remember to undo.
- **The fixed slots go.** A move carries its own path, so `policy.kick_left` stops being a
  config key and `robotctl policy load kick_left <file>` has no slot to name.
- **Pending skills are an `AtomicU32` bitmask** in `robotd::intents`, so a skill becomes an
  index. Fine to 32, worth knowing it is there.
- **Every skill's network is loaded and warmed at controller build** — seven sessions today.
  An open list means twenty community policies is twenty sessions and twenty warm-up
  inferences at startup. Lazy-loading on first trigger is not the answer, since opening a
  session is tens of milliseconds inside a 20 ms tick, so eager stays right and there is a
  practical ceiling.
- **Binding a skill to a pad button** is the next thing after this and is deliberately not
  part of it: it touches a working input path, and it is far easier to get right once there is
  something real to bind.
