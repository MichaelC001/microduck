# The Policy Channel — policies from the Hub

Status: draft · Date: 2026-08-31 · Owner: pierre

Where the ONNX policies come from, how someone tries one they did not train, and what
"reset" puts back.

**Built so far** (2026-08-31): §3, §4, §5, §9, §9.1 and §9.2 — the whole of §7 except `check`
covering community entries. A policy on the board can be tried and undone without editing a file or
restarting anything; the official set is fetched from the Hub rather than carried by the release;
and a retrained gait reaches a robot without a daemon release.

**Not yet true**: `policy check` covers the official set only — a community policy fetched from a
moving branch records the commit it came from, but nothing yet compares that against the branch to
say it has moved. Everything else in this page is built.

Companion to [`updater-design.md`](updater-design.md), which owns the update engine —
components, sources, signing, the health gate, rollback — and to
[`robotd-design.md`](robotd-design.md) §2.3, which owns how a policy is validated and run.
This page owns the **channel**: which file fills a slot, who published it, and the commands
that change that. Where a mechanism belongs to one of those pages, this one says a sentence
and points.

## 1. What is wrong today

All nine `.onnx` files ship inside the daemon artifact, and `robotd` loads them from
`/opt/robot/daemon/current/policies/`. Three consequences, and `policies/README.md` has named
them since the day it was written:

- a gait retrain needs a daemon release;
- a daemon fix re-downloads 6 MB of unchanged weights;
- a policy trained on a laptop reaches a duck only through CI or a sideload of the whole
  daemon.

Two asks follow from that, and they are **not the same feature**:

1. our own policies version independently of the daemon;
2. someone can try a policy they did not train, find out whether it is any good, and get back.

The first is an ordinary component (§5.5 of the updater design) and needs no new concepts. The
second is what this page is mostly about, because it is the one the component model does not
already cover.

## 2. Three origins, one slot

A slot — `walk`, `stand`, `sitstand`, `ground_pick`, `kick_left`, `kick_right`, `roulade` — is
filled from exactly one of three origins:

| | comes from | provenance | signed | auto-updates | reset target |
|---|---|---|---|---|---|
| **official** | the `policies` component | manifest, semver | yes | yes, per `auto_apply` | **yes** |
| **community** | any other HF repo | repo + revision + commit sha | no | no — reported only | no |
| **local** | a path on the board | none | no | no — unknowable | no |

Origin is decided by the HF org: **`pollen-robotics/*` is official, everything else is
community**. One constant, one place. It is not a config key — a robot that can be told which
org to trust is a robot whose "official" badge means nothing.

Origin drives behaviour and not only a label:

- **official** is what `policy reset` returns to, and the only origin the periodic check may
  ever apply on its own;
- **community and local** are never auto-applied and never a reset target, and carry their
  origin everywhere they are displayed — `policy list`, `policy check`, and the policy name
  `robotctl monitor` prints, which today says `walk` for gaits that share nothing but a slot.

**Signature verification is deliberately not required for community policies.** Every other
artifact the engine installs is verified against a trusted key, and this is the first exception.
The argument for it is that a policy is not a binary: `robotd` holds the only write handle to
the bus behind joint clamps, a fall→limp reflex and an intent deadman
([`robotd-design.md`](robotd-design.md) §2.4), and the `obs[1,61] → actions[1,14]` gate refuses
a wrong-shaped graph while the robot is standing still. That sandbox is the boundary, not the
signature. A daemon binary has no such sandbox, which is why the component path's rule does not
move.

## 3. Slots are config, so `load` and `reset` are config edits

`policy.walk` is already `Option<PathBuf>` in `robotd.toml`, and unset already means "resolve to
this mode's default" (`robotd-params/src/lib.rs`, `PolicyParams::resolved`). `robotctl
configure` already sets *or removes* exactly the keys it touches, through `toml_edit`, without
disturbing a comment. So:

- `policy load <slot> <x>` writes `policy.<slot>`, then reloads live;
- `policy reset <slot>` removes the key, then reloads live;
- `policy reset` removes all seven — "put it back the way it came".

That is the whole persistence model, and it is not new machinery. A load survives a reboot,
which is what [`updater-design.md`](updater-design.md) §5.7 already requires: "active model
selection" is a **user preference**, kept in a config file the updater never overwrites and
preserved across update *and* rollback.

**An ephemeral "try it until reboot" mode was considered and rejected.** It reads as the safer
default — a bad gait is one power cycle away from gone — but it puts a second source of truth
next to the config file for what the robot is running, and it makes `reset` almost meaningless,
since a reboot would already be the undo. One durable answer plus a one-word undo is easier to
explain and easier to be sure about. The risk it was guarding against is handled in §5 instead,
where it belongs.

## 4. The live reload

`robot.setMode` already swaps every ONNX session under a running 50 Hz loop, and a policy load
is that same path with a narrower scope. The flow (`robotd/src/main.rs`, the mode-switch
handler and its completion at the home pose):

```text
IPC call        → validate what this side can, set an intent, answer accepted
loop            → re-resolve the params, start loading the controller on a worker thread
                  (load + warm-up), and decide whether the change touches the network
                  that is driving
  if it does    → home the robot with torque on; swap in at home, fresh
  if it does not→ swap in wherever the robot is, carrying the running state across
on success      → publish the new policy names, resume
```

Three differences from a mode switch.

**Scope.** A load overrides one field of `policy_params` and re-resolves, exactly as the mode
switch sets `policy_params.mode` and re-resolves. Rebuilding the controller reloads all seven
sessions rather than the one that changed — on a worker thread, because a swap can now happen
under a moving robot and the loop cannot stall for the load.

**Only a change to the network driving moves the robot.** The first field report against this
design: load a walk, walk, sit, `policy reset walk` — and the robot ramped to home and stood up.
The reset was right about the config and wrong about the robot: `sitstand` had it, and the reset
did not touch `sitstand`. So the loop compares the change against what stepped last tick, by the
resolved path of that one network (the whole skill list when a skill is driving, since a skill is
an index into it). Unchanged, and the new controller is swapped in wherever the robot is, taking
over the old one's seat, skill and filter state, so the next tick is the same network from the
same place. Changed, and the robot goes home first as every change used to, holding there until
the load lands if it has not yet. A reload always counts as a change to whatever is driving: it
exists for the case where the paths are unchanged and the bytes are not, so the paths cannot
answer.

**A request that is already true does no work.** Resetting a slot nobody overrode, or loading
the file a slot is already running, is answered as an acceptance that queued nothing — the same
answer `robot.setMode` gives for "already in that mode", and for the same reason: the caller
asked for a state and has it. Without this, `policy reset` on an untouched robot sends it home,
reloads seven networks and comes back to exactly what it was running, which reads as a fault.

The one slot that is never already-done is one carrying an error. An override that failed at
boot has been dropped in memory (§5), so it reads as not-overridden and running the default —
indistinguishable from a slot nobody touched. Resetting it is how the error and the config line
behind it get cleared, so that request has to go through.

**Failure must not cost you the gait you had.** Today a controller that fails to build leaves
`policy_error` set, health unhealthy, and the robot holding its pose — right for a mode switch,
which is a statement about how the robot is configured, and wrong for a trial, which is
speculative by definition. Trying a 51-D file should not leave a robot with no gait until
someone restarts it. So a load **keeps the running controller** when the new one fails, and
returns the reason to the caller. The gate that produces the reason already exists and already
runs at the home pose before anything goes live; this is about what is done with its error.

## 5. A bad override must not gate daemon updates

Persistence has one sharp edge. A community or local policy that loaded when it was tried can
fail at the *next boot* — the file was deleted, the library was wiped, the board was reflashed
with the config restored. `robotd` would then report **unhealthy** on every start, and
`updaterd`'s health gate would roll back daemon releases for a reason no release caused: an
update loop caused by a missing file no update could have supplied.

The rule that fixes it:

- a **community or local** override that fails to load falls back to that slot's **official**
  default and reports **degraded**, naming the file;
- an **official** policy that fails to load stays **unhealthy**.

This is the distinction `HealthResult::degraded` exists for. A missing override is a property of
the board, not of the release being gated — reverting the daemon cannot fix it, and would only
churn the boot counter. A broken official policy genuinely is a broken bundle, and rolling it
back is the correct response.

Note this does **not** silently repair the config. The override stays written and
`robotctl health` says which file could not be loaded, so the state is visible and `policy
reset <slot>` is the fix. A daemon that quietly rewrote config on a bad boot would be a worse
surprise than a degraded robot that says why.

## 6. Versions, and checking for a newer one

A community policy held as "a file in a library" has no version and nothing to compare against,
so the library keeps a small sidecar per entry: repo, revision, resolved commit sha, file name,
fetch time. That record is what makes checking possible at all, and most of what reads it
exists — `source/hf_hub.rs` already parses the refs API into `RepoRefs { tags }`, and
`api/models/{repo}` gives `sha` and `lastModified`.

Versions are rendered by what the publisher actually offers, rather than by pretending everyone
uses semver:

| origin | shown as |
|---|---|
| official | the manifest's semver — `1.2.0`, like any component |
| community, repo has `v*` tags | the tag |
| community, tracking a branch | short sha + date |
| local | `local`, and `check` reports unknown |

**`policy check` is the one place to look.** It routes internally — official slots ask
`updaterd` about the `policies` component, community slots ask the Hub — because "which command
tells me whether my walk is out of date" having two answers is exactly the confusion worth
spending a little routing to avoid.

The periodic check (`check_interval`, 6h) covers community policies too, and **reports without
applying**. That is already how `auto_apply` treats an ordinary release: availability is a fact
to surface, installing is a decision. It is also what lets the app say "an update is available
for your bouncy walk" later with no new plumbing.

## 7. The command surface

```text
robotctl policy list                      slot · current · origin · version
robotctl policy check [slot]              is anything newer at the source
robotctl policy load <slot> <repo|name|path>
robotctl policy update <slot>             fetch the newest and load it
robotctl policy reset [slot]              back to official; no slot = all seven
robotctl policy search <query>            Hub models matching a query
```

`load` takes an HF repo (`org/name[@rev]`), a name already in the library, or a **path on the
board**. The path form is the trainer's `scp`-then-try loop and is the reason the whole feature
is worth having on a bench; it is also the one input with no provenance, which is why `local` is
a first-class origin in §2 rather than an accident.

There is no separate `fetch`: `load` downloads when it has to, and the library accumulates, so
loading A, then B, then A again costs one download each. `search` is thin — `api/models?search=`
against the query — and until a `microduck` tag exists on the Hub, searching for the word is
what there is.

## 8. Wire, and what it costs

Two new methods on `robotd`: `robot.loadPolicy` (slot + source, answered like `robot.setMode` —
accepted, refused with a reason, or a no-op) and `robot.policies` (what each slot runs, with
origin and version). One new namespace on `updaterd`, `policy.*`, for the fetch.

`API_VERSION` goes 16 → 17. `robotctl` and `btd` ship in the same artifact as `robotd`, so they
move together; a client pinned to v16 gets `METHOD_NOT_FOUND` naming the method, which is the
designed skew behaviour and not a handshake refusal
([`duck-ipc-proto/src/lib.rs`](../../duck-ipc-proto/src/lib.rs), `API_VERSION`).

**The fetch lives in `updaterd`, not in `robotctl`.** `robotctl/Cargo.toml` states the rule it
would break: a support tool on the recovery path does not link the update engine's
http/tar/zstd/crypto tree, and structurally cannot now. `updaterd` already has the HF URL
resolution, a `reqwest` client and the progress notifications the app will want. The cost is
honest — it is the first unsigned download inside the daemon whose invariant is that no unsigned
bytes reach a live path — so the `policy.*` namespace can write **only** into the library
directory and never into a component's install dir, which keeps that invariant literally true
where it is load-bearing.

The library lives at `/var/lib/robot/policies/`, outside every release directory, per
[`updater-design.md`](updater-design.md) §5.7 rule 1.

## 9. Leaving the artifact

`robotd` reads its policies from `/opt/robot/policies/current`, not from inside the release. That
much is done, and it is the part that matters: there is one runtime source for a policy, and no
precedence rule between a release copy and anything else.

**What fills that directory is `scripts/seed-policies.sh`, and it downloads.**
Run by `hooks/postinstall` on every update and by `install.sh` on a fresh board, it fetches the
pinned set from the Hub into `/opt/robot/policies/releases/seed-<pin>/` and points `current` at
it — the same `releases/` + `current` symlink layout the updater already swaps (§7.1 of the
updater design).

Fetching rather than copying is the point: it is the arrangement `setup-board.sh` already uses
for ONNX Runtime and `setup-gstreamer.sh` for the plugins, which are the other two things a board
needs and a release has no business carrying. The pin lives in `[workspace.metadata.policies]`
and as literals in the script, with a test asserting they agree — `setup-gstreamer.sh`'s trap,
because a script that runs from inside a release cannot read the manifest.

**The pin is a floor, not a ceiling**, and that distinction is load-bearing. It ships inside the
daemon release, so bumping it *does* need a daemon release — an earlier draft of this section
claimed otherwise and was simply wrong. What the pin decides is what a *freshly provisioned* board
installs. Moving past it is `robotctl policy update` (§9.1), which is the thing that makes a
retrained gait reach a robot without a daemon release, and therefore the thing that makes this
whole channel worth having.

Three things it does not do. It does not re-download a set it already has, so an update whose pin
is unchanged touches no network — which matters because the post-install hook runs under a
120-second timeout and a hook that times out rolls the update back. It never installs a partial
download: everything lands in a staging directory and `current` moves only once the whole set has
arrived. And it does not replace a working set with an older one when a fetch fails — a
half-published revision leaves the board on the gait it had, and the pin is retried at the next
update.

Nothing is signed, and that follows §2 rather than contradicting it: this is the same download a
person could make, into a directory `robotd` shape-checks everything out of. The shape gate is
also what catches a truncated file, which is why no hashes are pinned here to go stale on every
retrain.

One rule makes that a bootstrap rather than a second home: **a set that is already installed is
never replaced, whatever the pin says**. There are two states — something is installed, or
nothing is — and only the second one fetches. Everything after the first install belongs to
§9.1.

That rule got stricter after a board proved the looser one wrong. It used to replace an older
`seed-*` on the reasoning that a daemon update was still how a retrained gait reached a robot.
`policy update` is now how, and the old rule became a trap: a board moved forward to `v2` by hand
has `current -> releases/seed-v2`, which matches the pattern the seeder called its own, so the
next unrelated daemon update would have put `v1` back — reverting the gait somebody chose, as a
silent side effect of a binary update.

**Policies no longer roll back with the daemon**, and that is worth stating plainly because it
used to be free. While they lived inside the release, reverting the release reverted them; now
`current` is repointed by the postinstall hook, and rollback does not run hooks
(`post_swap` is on the apply path only). So a release rolled back *because a policy in it was
bad* leaves that policy running. Two ways back, and the first is the ordinary one: a forward
`robotctl update apply daemon --version <older>` runs the hook and reseeds. Failing that, the
seed the release replaced is still on the board — the seeder keeps the previous one for exactly
this — and `current` can be pointed at it by hand.

This is the intended shape rather than a regression: two things with their own version lines do
not revert together. It is only sharp while the release is still the only source of policies,
which is another reason not to leave that state sitting for long.

**A board that cannot reach the Hub on a first install has no gait**, and that is the accepted
shape rather than an oversight. `robotd` holds its pose and reports *degraded*, so the update gate
passes and nothing rolls back; the next update fetches. It is the same bargain `setup-board.sh`
already makes for ONNX Runtime and `setup-gstreamer.sh` for the plugins — a board needs a network
once, and this is one more thing that needs it then rather than a reason to carry six megabytes in
every daemon release forever.

What the seeder must never do is *fail*: a non-zero exit from the post-install hook rolls the
update back, so a network problem would revert a release that had nothing to do with it. Every
error path exits zero and says so on stderr.

**One component for the whole set, not one per slot.** The updater design sketched
`model-walk`, `model-jump` and so on (§5.5), and per-slot components are what its own machinery
would give most naturally. Against that: the nine files are produced as a *family* by one
training run, the slot→file mapping is mode-dependent (`walk` is `alpha_walking.onnx` on legs
and `roller.onnx` on wheels, which postdates that section), and nine components means nine
repos, nine signatures, nine config blocks, nine round trips per check, and a nine-dimensional
skew matrix in which nothing records that a given walk and stand were ever trained together. One
component means a mode switch downloads nothing and the set is versioned the way it is built.

What one component gives up is rolling back a single slot, and that is exactly what the per-slot
override in §3 already covers — from any origin, which a per-slot component would not have
managed either.

**A missing policy is degraded, not unhealthy**, by the same §5 rule: a freshly provisioned board
that has not fetched the set yet has no gait, and that must not fail the health gate of every
subsequent daemon update. Provisioning installs the bootstrap set, where `setup-board.sh`
already installs the ONNX runtime and `setup-gstreamer.sh` the plugins — the network dependency
lands where one exists, and at runtime there is exactly one source for a policy with no
precedence rule between a release copy and a Hub copy.

### 9.1 Moving past the pin

`robotctl policy check` asks the Hub what revisions the set's repo offers, against the one on the
board. `robotctl policy update` installs one — the newest by default, or `--version v1` to go
back — and tells `robotd` to re-read its slots.

Both are served by `updaterd` (§8), and three details are the whole of why this is not simply a
second copy of the seeder:

**The repo comes from the set, not from configuration.** Each installed set carries a `.source`
record naming the repo and revision it came from, written by whatever installed it. So there is no
second place to configure the repo and nothing to drift; a set installed by some future tool
answers the same question the same way.

**Newest means the repo's own newest, not a semver sort.** A policy repo is not obliged to use
semver, and ordering `bouncy-2` against `v10` would be a guess presented as a fact. The Hub lists
refs oldest-first, so newest is that reversed, and `check` prints the whole list rather than only
its own verdict — going *back* is as much the point as going forward.

**`robot.reloadPolicies` exists because the paths do not change.** Installing a set swaps
`current` underneath every slot, so each one still resolves to the same string and
`robot.loadPolicy` would correctly conclude there is nothing to do. It is right about the paths
and wrong about the bytes, which is exactly the case that needs a separate method — and one that
is never short-circuited, since "already loaded" is the answer it exists to disbelieve.

A reload changes no configuration, so **a slot you loaded yourself survives it**. That is not a
nicety: the reload runs at the end of every `policy update`, and an install that quietly discarded
someone's gait experiment would be the worst possible time to discover the two were conflated.

An unreachable Hub is reported, not raised. The robot is walking either way, and a caller shown
"could not reach the Hub" beside what is installed knows more than one shown an error. Neither is
a board with nothing installed: that is not a network fault, and saying it through the same field
made a healthy Hub look unreachable.

Installs prune to the live set and the one it replaced — seven megabytes each, and somebody
comparing two gaits will move back and forth. The predecessor is kept on purpose: rollback does
not run hooks, so reverting the daemon does not revert its policies, and pointing `current` back
at the kept set is the recovery when a policy is what went wrong.

### 9.2 Somebody else's policy

`robotctl policy load walk RemiFabre/microduck-flamingo-cycle` fetches one policy from any Hub
repo into `/var/lib/robot/policies/<org>/<name>/<revision>/` and puts it in that slot. Outside
every release directory, per §5.7 of the updater design: a policy somebody chose survives an
update and a rollback.

**The path is made of the answer**, which is what lets `origin` be honest without a lookup: the
org that published a policy is a component of where it lives, so `robot.policies` reports
`community` for a stranger's and `official` for ours. It is a label and not a boundary — anyone
who can edit `robotd.toml` can name a directory whatever they like, and anyone who can do that
can run whatever they like anyway. What stands between a bad policy and a broken robot is §2's
sandbox.

**A repo carries one `policy.onnx`.** That convention already existed on the Hub before this was
built — every published microduck policy has exactly one `.onnx` beside a README and a manifest —
so the fetch takes the sole `.onnx` and refuses a repo with several by naming them. Choosing wrong
means running the wrong network on a real robot, which is not a coin to toss;
`<repo>:<file>` says which.

**And it carries a `manifest.json`**, which turned out to be a richer convention than anything
this design had specified: `obs_len`, `action_len`, `model_api`, `robot.model`, plus a name, a
kind and a description. Reading it is what lets a policy be refused *before* 800 KB is downloaded
and before the robot is asked to run it — "its manifest says observation width 51, and this robot
builds 61" is the same verdict `robotd` would reach at load, arriving where somebody can act on
it. This is also where `model_api` finally does something: §5.5 of the updater design specified
it and neither side used it, and now a policy needing a newer daemon is refused with that as the
remedy.

Three rules keep that from becoming a trap. The manifest is **untrusted** — a stranger's
description of a stranger's file — so it is a reason to refuse and never a reason to trust; a
manifest that lies is caught by the shape gate, which is where the real check has always been.
**Absence is not evidence**: a repo with no manifest, or one omitting the fields we act on, is
accepted, because most of the Hub follows no convention of ours and refusing on silence would
reject the majority of it. And the numbers it is checked against are published in
`duck_ipc_proto` rather than duplicated, with a compile-time assertion in `duck_control` that the
two agree — a contract with whoever publishes a policy belongs where both sides can see it.

**A slot can be switched off**, with `none` — the literal `[policy] <slot> = "none"` already
used. Every slot but `walk`, which is what the others fall back to and so cannot be empty; asking
is refused, and a config that already says it is repaired at startup and reported degraded. Not an afterthought: the first community policy anyone will try does its own two-foot
stand, and `will_stand` hands the robot to the standing network whenever command magnitude is
zero, which is exactly the state that policy is in when it is standing on two feet. Without a way
to say `stand none`, running it meant editing the file this whole command exists to stop editing.

`robotctl policy search microduck` lists what is out there, marking each hit's origin. No tag
filter: a shared name is what the published policies have in common, and a tag is worth adding
once there is something to tag.

### 9.3 The set describes itself

`manifest.json` at the root of the policy repo says what the set contains and what each policy
is. It lives **only on the Hub** — there is no copy in this repository, deliberately. A copy here
would be a second source of truth for something that versions on the Hub, and a test over it
would pin the copy rather than the thing a robot downloads, which is worse than no test: it
passes while a board gets something else.

Two lists used to be hardcoded, and between them they meant a tenth policy in the set was a
daemon release rather than a tag:

- `scripts/seed-policies.sh` knew which files to download. It now reads them from the manifest,
  keeping the nine it knows as a fallback for a revision tagged before the manifest existed.
- `robotd-params` knew which policies were one-shot skills and how long each ran. It now takes
  them from the manifest, falling back the same way.

Three kinds, and `kind` says who ends the policy:

| `kind` | means | a skill? |
| --- | --- | --- |
| `perpetual` | runs until told otherwise — a gait, or a hold a person has to end | no |
| `episodic` | runs for `duration_s` and returns itself to a safe pose | **yes**, on a constant command |
| `scripted` | episodic, but interruptible — the daemon can change its command mid-flight | no |

What the daemon *feeds* a policy is a separate axis, `command.encoding`, and it decides what an
episodic entry becomes. On a constant command (the default, `idle` on the way back) an episodic
policy is a **skill**: kicks, roulade, every community one-shot so far. On a `phase` command it is
the **ground pick** — the daemon writes `[cos 2πφ, sin 2πφ, 0]` with φ advancing over `period_s`
and hands back at `end_phase`; the entry's numbers become that mode's ground-pick defaults, and it
is not a skill, because a generic one-shot would feed it zeros. The `posture_flag` encoding is
the sit↔stand: `scripted`, because dropping the flag mid-descent is a legitimate thing to do,
with `unwind_s` the length of the rise and `ramp_s` how long the seat takes to settle.

`mode` tags a policy as one drive mode's — absent means walking — which is how the roller crouch
is the ground pick of roller mode rather than a second one for walking. `name` defaults to the
file's stem, so only a policy whose role differs from its training run needs one:
`ball_kick_left.onnx` answers to `kick_left`, and `roulade.onnx` says nothing.

**The per-policy fields are the same ones a single-policy repo uses**, plus `file`. That is what
makes the ask to a community publisher "add these fields" rather than "adopt our format", and it
means one reader understands both shapes.

### 9.4 What the set's manifest says, and adding to it

The file at the root of `pollen-robotics/microduck-policies`, in full. Shown here rather than
checked in: a copy in this repository would be a second source of truth for something that
versions on the Hub, and a test over the copy would pass while a board downloaded something else.

```json
{
  "schema_version": 1,
  "model_api": 1,
  "obs_len": 61,
  "action_len": 14,
  "robot": { "model": "microduck", "hw_rev": 1, "servos": "xl330", "control_hz": 50 },
  "description": "The policy set a microduck ships with: walking, standing, and the one-shots its buttons run.",
  "policies": [
    { "file": "alpha_walking.onnx", "kind": "perpetual" },
    { "file": "alpha_stand.onnx",   "kind": "perpetual" },
    { "file": "roller.onnx",        "kind": "perpetual", "mode": "roller", "action_scale": 0.8 },

    { "file": "alpha_sitstand.onnx", "name": "sitstand", "kind": "scripted",
      "command": { "encoding": "posture_flag", "slot": "twist.vx", "sit": 1.0, "stand": 0.0, "idle": [0.0, 0.0, 0.0] },
      "ramp_s": 2.0, "unwind_s": 1.0 },

    { "file": "alpha_ground_pick.onnx", "name": "ground_pick", "kind": "episodic", "duration_s": 2.8,
      "command": { "encoding": "phase", "slots": "twist.vx,twist.vy", "period_s": 4.0, "end_phase": 0.7 } },

    { "file": "roller_crouch.onnx", "name": "crouch", "kind": "episodic", "duration_s": 3.5, "mode": "roller", "action_scale": 0.8,
      "command": { "encoding": "phase", "slots": "twist.vx,twist.vy", "period_s": 5.0, "end_phase": 0.7 } },

    { "file": "roulade.onnx",         "kind": "episodic", "duration_s": 1.0, "chain": true },
    { "file": "ball_kick_left.onnx",  "name": "kick_left",  "kind": "episodic", "duration_s": 0.5 },
    { "file": "ball_kick_right.onnx", "name": "kick_right", "kind": "episodic", "duration_s": 0.5 }
  ]
}
```

Read that against what a robot does with it. The nine `file` entries are the download list, so
the seeder fetches exactly these. The three `episodic` entries on a constant command become
skills — which reproduces the built-in three exactly, and is the check that the manifest is
*right* rather than merely plausible. `ball_kick_left.onnx` carries a `name` because its role
differs from its training run; `roulade.onnx` does not, because the file's stem is already the
name.

The two `phase` entries are the ground pick of each mode, and their numbers are what the daemon
used to carry as literals: a 4 s cycle for the pick, and — the correction this shape paid for —
a **5 s** cycle for the crouch, which is what `Mjlab-RollerCrouch` trains on and not the 3 s the
roller preset had inherited from the prototype. `duration_s` is `period_s × end_phase`, written
out so a reader need not multiply. `[policy] ground_pick_period` and `ground_pick_action_scale`
still override the set's numbers, because the config file is the list of a person's decisions.

The `scripted` sitstand is recorded, not turned into a skill: it is driven by the sit toggle, the
shutdown sit and the seated-boot rise, through a flag the daemon flips. Its `unwind_s` is how long
the rise runs on the sitstand network before the gait takes over (the daemon's `RISE_SECS`, now
the set's to say), and `ramp_s` how long the seat takes to settle — the shutdown sit waits twice
that before cutting torque, which is the prototype's four seconds over its 2 s glide.

`action_scale` on a perpetual entry (`roller.onnx`) is recorded and not read: the gait's scale
resolves per mode from `[policy]`, and the manifest has no way to say which perpetual is the
walking slot and which the standing one.

**Adding a policy to the set** is then four steps and no daemon release:

1. Upload the `.onnx` to the repo.
2. Add an entry to `manifest.json`. `kind` and `command.encoding` decide what happens next:
   `episodic` with a `duration_s` on a constant command becomes a skill a robot can be asked for
   by name; `episodic` on a `phase` command is a mode's ground pick and sets its timing;
   `perpetual` is a gait, which needs a slot pointed at it; `scripted` is recorded, and the
   daemon's own arm for it reads its timing.
3. Tag the revision — `hf repos tag create pollen-robotics/microduck-policies v4`.
4. On a robot: `robotctl policy update`.

A new episodic policy is then `robotctl robot do <name>` and can go on a button, with nothing
edited and nothing rebuilt. `[workspace.metadata.policies]` in the root `Cargo.toml` decides
which tag a *freshly provisioned* board installs, and bumping that does need a release — the pin
is a floor, not a ceiling.

**One guard is on the board, because it cannot be anywhere else.** A set entry may not answer to
`ground_pick` or `sit_toggle`: those have their own arm of the cascade, and a second network
behind either name would be fed an all-zero command it was never trained on. Nothing in this
repository can check a file that lives on the Hub, so the check runs where the file is read.

## 10. Skills: what a robot can be asked to do

A slot is what the robot runs *by default*. A **skill** is what it runs when asked — the kicks,
the roulade, and anything else on the same shape.

Adding one used to touch seven places: the `Skill` enum, `Net`, `PolicyPaths`, a `Policy` field
and its `has_*`, a `Slot` with a config key and a registry entry, a branch in the control loop,
and padd's button table. So a community policy shaped exactly like the roulade — zero command,
four seconds, selecting it is the trigger — could not be added without a daemon release.

It was seven places because it did not need to be one. These two arms were the same arm:

```rust
} else if let Some((left, _)) = self.kick {
    let net = if left { Net::KickLeft } else { Net::KickRight };
    (net, Command::default(), label)
} else if self.roulade.is_some() {
    (Net::Roulade, Command::default(), "roulade")
```

Kicks and roulade differed in four numbers — duration, action scale, gain ratio, and whether
holding the button chains another — and in nothing else. So they are four numbers:

```toml
[[policy.skill]]
name = "polite-bow"
path = "/var/lib/robot/policies/fffiloni/microduck-polite-bow-b1d864/main/policy.onnx"
duration = 4.0
```

`robotctl policy add polite-bow <repo>` writes that, taking the length from the repo's manifest.
Absent means the built-in three, and an entry merges by name, so a board updates onto this with
no config and no migration — and adding one cannot silently remove another by omission.
`"none"` removes a built-in, the same word that switches off a policy slot.

### 10.1 Who supplies the ending

`kind` in a manifest is not about how long a policy runs. It is about **who ends it**.

An *episodic* policy returns itself to a safe pose — `polite-bow` is standing again after four
seconds — so the window can simply expire and the gait takes over an upright robot. A *perpetual*
one holds until told otherwise, and expiring on it would hand the gait a robot balanced on one
foot. So a skill may declare both halves:

```toml
command  = [1.0, 1.0, 0.0]   # the twist while it runs — flamingo reads [flag, side, 0]
unwind   = [0.0, 1.0, 0.0]   # what it drives on the way back
unwind_s = 3.0
```

That is the daemon supplying the ending the policy does not have, and it is the shape the sit
toggle already had: latched, then a timed rise before handing back, because dropping the flag
does not instantly make a robot stand. Both default to nothing, so the common case declares
neither.

Head and body are zeroed for every skill, whatever the phase — every one-shot published so far
declares them unused, and a policy trained with `zero_command_padding` expects exactly that. Only
the twist differs, and for most skills it is zero too.

**A skill's twist is unsmoothed by construction.** The EMA is applied to the *client's* command on
its way in, and a skill never reads that — the loop builds a fresh command block. Which is why a
policy reading a flag rather than a velocity needs no `cmd_alpha` as a skill, and needed it set
globally when it was squatting in the walk slot.

### 10.2 What stays in the daemon

`walk` and `stand` are the fallback pair, chosen by command magnitude, and there is nothing below
them to hand back to. `sitstand` is latched and driven internally by the shutdown sit and the
seated-boot rise, not only by a button — `scripted`, in the manifest's word. `ground_pick`
writes a phase rather than a constant. None of the four is a generic one-shot, and a set entry
may not answer to `ground_pick` or `sit_toggle` — a second network behind either name would be
fed an all-zero command it never trained on. The guard is on the encoding as well as the name: a
`phase` or `posture_flag` entry is never loaded as a skill however it is called. What the set
*does* get to say about these arms is their timing (§9.3) — the pick's cycle and cutoff, the
rise and the seat's settle — since those are properties of the trained network, not of the build.

**A running skill has the fall reflex off** for its whole duration: the limp-fall predictor is
only consulted while the controller is not `busy()`, and any active skill makes it busy. That was
uncontroversial when every one-shot was under a second. A skill that can be configured to hold
for ten is a robot with no fall reflex for ten seconds, and that wants deciding rather than
inheriting.

### 10.3 The button

`[pad]` says which of the five one-shot buttons runs which skill:

```toml
[pad]
x = "polite-bow"
```

`robotctl pad bindings` shows them, `pad bind` changes one, `pad reset` puts them back. The
defaults are the mapping the prototype had, so a robot with no `[pad]` behaves as it always has,
and `padd` re-reads the file within a second — nothing restarts.

Only those five. `Start`, the two stick-mode toggles, held `Select` and held `D-pad up` are not
`robot.do` calls, and the button that powers a robot off is the one binding worth not being able
to lose to a config edit.

**Over the wire, `pad.bindings` and `pad.bind` are `robotd`'s**, not `configd`'s, which owns the
rest of `pad.*`. Pairing is about the radio; a binding is about what a button does to the robot,
and answering it needs the list of skills this robot has so a name can be refused rather than
becoming a dead button. Routing is per method throughout — `policy.*` goes to `updaterd` while
`robot.loadPolicy` goes to `robotd` for the same concept — so the split costs nothing mechanically
and is only worth naming because the namespace suggests otherwise.

A binding is checked against `do_names`, which is **not** `policy.skills`: `ground_pick` and
`sit_toggle` have their own arm of the cascade rather than being config entries, so they are
absent from that list while being perfectly good things to ask for. Validating against `skills`
alone rejected two of the five buttons the pad ships bound to, which a test caught.

**The skill table is reachable over both transports too**, which is what closes the path: fetch a
stranger's policy, give it a name and a length, ask for it by that name, put it on a button — none
of it needing a terminal on the robot. `robot.setSkill` writes the config and reloads inside the
one call, and `robot.skills` reports `built_in` separately because `ground_pick` and `sit_toggle`
are not table entries and a client reading only the table would conclude they do not exist.

`padd` still knows nothing about what a skill *is*. It reads which button went down, looks up the
name beside it, and sends that name; `robotd` decides whether the robot has such a thing and
answers with the list it does have when it does not. Checking belongs where the answer is, which
is why `robotctl pad bind` asks the robot and refuses a typo with the real list.

## 11. What the official set currently is

Recorded here because it lives nowhere else in this repository now that the files do not, and
because the mapping is not recoverable from the names on the Hub. Copied from
`apirrone/microduck_runtime` at `5f3b314` (`roulade.onnx` at `7e4ab6d`, where it first appeared),
dereferencing the symlinks that repository uses to give stable names to particular training runs:

| in the set | upstream | role |
| --- | --- | --- |
| `alpha_walking.onnx` | `BEST_alpha_walking_rough.onnx` | walking / velstand |
| `alpha_stand.onnx` | `BEST_alpha_stand_body_control.onnx` | standing + body-pose |
| `alpha_sitstand.onnx` | `BEST_alpha_sitstand.onnx` | sit ↔ stand (posture flag) |
| `alpha_ground_pick.onnx` | `alpha_ground_pick.onnx` | ground pick (phase command) |
| `ball_kick_left.onnx` | `ball_kick_left.onnx` | left-leg kick |
| `ball_kick_right.onnx` | `ball_kick_right.onnx` | right-leg kick |
| `roller.onnx` | `BEST_roller.onnx` | roller-mode locomotion |
| `roller_crouch.onnx` | `BEST_roller_crounch.onnx` | roller-mode crouch (ground-pick slot) |
| `roulade.onnx` | `roulade.onnx` | forward roll (Mjlab-Roulade-MicroDuck) |

(`roller_crouch` also fixes the upstream file name's typo.)

**The names are roles, not training runs**, and that indirection is the reason a retrain is a pin
bump rather than a config change on every robot: swapping which run is "the walking policy" must
not mean editing `robotd.toml`.

**Only the 61-D family.** The prototype also ships a 51-D one — `3 gyro + 3 gravity + 42 joints +
3 command`, the legacy `[vx, vy, vtheta]` — and `robotd` refuses it at load naming both widths.
That check turned a wrong-policy mistake into a diagnosis rather than a robot moving in ways
nobody could explain, and it is now also what catches a truncated download (§9).

## 12. Naming

The updater says `model`; `robotd` says `policy`. Standardising on **policy** — the component is
`policies`, the namespace is `policy.*`, the commands are `robotctl policy …`. `models/` keeps
its meaning for the things that genuinely are models and not control policies, such as
`pet_detect.onnx` and the duck detector.

## 13. Decisions recorded

| | |
|---|---|
| Load and reset are config edits plus a live reload | One source of truth for what the robot runs; reset needs no new concept (§3) |
| No ephemeral trial mode | A second source of truth, and it makes `reset` redundant with a reboot (§3) |
| A failed load keeps the running controller | A trial is speculative; it must not cost the gait you had (§4) |
| A request already satisfied queues no work | Otherwise `reset` on an untouched robot is a ten-second non-event (§4) |
| …except on a slot carrying an error | A fallen-back slot looks untouched, and clearing it is what reset is for (§4) |
| A failed community override is degraded, not unhealthy | Otherwise a stale config gates every daemon update (§5) |
| `pollen-robotics/*` is official, hardcoded | A configurable trust org makes the badge meaningless (§2) |
| Community policies are not signature-verified | The safety layer and the shape gate are the boundary, not the key (§2) |
| One `policies` component, not one per slot | The set is trained as a family; per-slot overrides already cover the rest (§9) |
| The fetch lives in `updaterd` | `robotctl` must not link an HTTP stack (§8) |
| Policies live outside the release, seeded by it | One runtime source, and no precedence rule to get wrong (§9) |
| Seeding never overwrites a set it did not install | The handover needs no flag: the first real install ends it (§9) |
| The set is downloaded, not shipped | Same as ONNX Runtime and the plugins; bumping the pin ships a gait (§9) |
| A failed fetch keeps the set already installed | A half-published revision must not downgrade a working gait (§9) |
| The pin is a floor; `policy update` moves past it | Otherwise a gait still needs a daemon release, which is the thing this channel is for (§9.1) |
| A set records the repo it came from | One writer, one copy, nothing to configure twice or drift (§9.1) |
| Reload is a third thing, not reset-all | They look identical from outside and conflating them discards every override (§9.1) |
| One-shot skills are config, not code | Kicks and roulade were the same arm with different numbers; a community one is a fifth set (§10) |
| `kind` says who ends a policy, not how long | An episodic one returns itself; a perpetual one needs the daemon to drive it back; a scripted one is episodic but interruptible (§9.3, §10.1) |
| `command.encoding` says what the daemon feeds it | Constant → a skill; `phase` → the ground pick; `posture_flag` → the sit↔stand. Only the first is loadable as a one-shot (§9.3) |
| The set carries its own timing | The pick's cycle and cutoff, the rise and the seat's settle are properties of the trained network, not literals in a build; `[policy]` keys still win (§9.4) |
| Skills leave walk, stand, sitstand and ground_pick alone | The fallback pair, one driven internally, one phase-driven — none is a generic one-shot (§10.2) |
| Buttons are config; Start and Select are not | The button that stops a robot is the one worth not being able to lose (§10.3) |
| `robot.do` is not teleop, so BLE may carry it | One request, not a stream; and it needs no control link, the deadman zeroes the twist (§10.4) |
| Loading a policy is served on both transports | Every refusal said it was waiting for a client; there is one (§10.4) |
| Installing from the Hub is not | It reaches the network and writes the eMMC, where loading points at a file already there (§15) |
| The seeder never replaces an installed set | Otherwise a daemon update silently reverts a gait chosen with `policy update` (§9) |
| Origin is the org in the path | Honest without a lookup, and a label rather than a boundary (§9.2) |
| The manifest can refuse but never bless | It is a stranger's claim; the shape gate is the check (§9.2) |
| A repo with two policies is a refusal | Guessing means running the wrong network on a real robot (§9.2) |
| Seeds are pruned to the current and previous | Unbounded 7 MB-per-release growth; the previous one is the hand-recovery after a rollback (§9) |
| One `policy check`, routing internally | Two commands for one question is the confusion worth avoiding (§6) |

## 14. Deferred, deliberately

- **Competing versions of one community policy.** A slot holds one file; the library holds many,
  but only the config says which is live. A real `(name, version)` store key is priced in
  [`updater-design.md`](updater-design.md) §17 at ~14 files and an `API_VERSION` bump, and the
  A/B case is served by loading each in turn.
- **Loading a whole set at once.** Per-slot only for now. A set matters if swapping between
  training-run families becomes routine, where mixing a walk from one run with a stand from
  another is a combination nobody trained.
- **A `microduck` tag on the Hub.** Searching for the word is enough until there is something to
  tag.
- **Signing community policies**, and any curated-org scheme that would require it.

### 10.4 Reaching it from a phone

`robot.do`, `robot.policies`, `robot.loadPolicy` and `robot.reloadPolicies` are served over
**both** BLE and WebRTC. What changed is not the safety of any of it — the shape gate, the joint
clamps and the fall reflex are the same whoever asked — but that a client now exists, which is
what every one of these refusals said it was waiting for.

**A skill is not teleop.** `robot.do` spent a while grouped with `robot.move` and friends under
BLE's transport argument — a 20-byte notification budget and a link that does not exist for the
first ~73 s of a boot. That argument is about a *stream*: fifty small updates a second. A skill is
one request, and it needs no control link at all, because the deadman zeroes the twist by itself
and a robot with nothing driving it stands still and bows.

**BLE is the transport that best meets the watching condition**, which is what most refusals here
turn on. Its radio reaches about ten metres, so whoever tapped the button is in the room with the
robot by construction — and the bond is PIN-checked with `encrypt_authenticated_write`. WebRTC
answers the same condition differently: the peer is watching the video, which is the argument
that already permits `robot.init` and `robot.shutdown`, and covers a gait better than it covers
standing up, because the peer is looking at the thing the gait is about to move.

The asymmetry worth remembering is the other way round from the intuition. **BLE is
authenticated; WebRTC is not** — §4 of `remote-webrtc.md` means any LAN peer inherits whatever is
opened, where BLE carries the same call from a PIN-bonded caller within ten metres. That is a
reason to sharpen §4, not a reason to withhold the call from the transport that can show somebody
the result.

**What limits the damage either way is that `robot.loadPolicy` does not persist.** §3 above
describes the persistence model as a property of `policy load` — the *command* — and that is
exactly right: `robotctl` writes `robotd.toml` and then calls the method. The method itself
mutates `robotd`'s in-memory params and reloads. So a gait chosen from a phone is gone at the
next restart or `robot.reloadPolicies`, which is the ephemeral "try it until reboot" mode §3
considered and **rejected** — arrived at here by accident rather than by decision. §15 has it as
open, because remote and local answering differently to the same words is a trap however the
question is settled.

**`robot.policies` carries the skill list**, and that is the piece that makes the rest usable. A
client cannot offer a bow without knowing the robot has one, and which skills exist is config now
— nothing to compile in. The names were already in `robot.subscribe`'s acknowledgement, but that
is a 50 Hz stream answering a question asked once, and BLE deliberately does not route it.

## 15. Open

- **`robot.loadPolicy` does not persist, and `policy load` does.** Same words, different
  durability: the command writes `robotd.toml` and the method does not, so a gait chosen from a
  phone is gone at the next restart. §3 rejected an ephemeral mode deliberately and this is one
  arrived at by accident. Either the method should write the file — which makes the wire the
  single path and removes `robotctl`'s duplicate half — or remote should say plainly that it is
  a trial. The first is more work and more consistent; the second is a defensible thing for a
  phone to do, but only if it is said.

  It also decides where the **pad bindings** can live, below: `padd` re-reads `[pad]` every
  second, so a `pad.bind` that did not write the file would be reverted within a second. There
  is no live-only option there, which means whichever daemon serves it needs the lossless writer
  that today only `robotctl` has.

- ~~Installing from the Hub is still local-only~~ — all four are served on both transports now.
  The reads (`policy.check`, `policy.search`) change nothing and sit beside the `update.check`
  both transports already carry. The mutations (`policy.install`, `policy.fetch`) are named one
  by one in `btd`'s `only_these_mutating_calls_are_reachable_over_ble`, which is the list that
  makes adding one have to say why.

  The uid gate turned out not to be the obstacle it looked like. `policy.install` is
  `is_mutating`, and `updaterd` authorises that against the *peer's* credentials — which are
  `btd`'s, not the phone's, exactly as they already are for `update.apply`. The transport is the
  gate there, not the credential.

  Adding a **skill** is reachable too, as of `robot.skills` / `robot.setSkill` /
  `robot.removeSkill`. That was the last thing here only a terminal on the robot could do:
  `[[policy.skill]]` is a repeating table, so `robotctl policy add` wrote it directly and there
  was no method to route. `robotd` writes the file and reloads itself, so one call is the whole
  operation — a client that had to remember `robot.reloadPolicies` and forgot would leave a robot
  whose config and behaviour disagree until the next restart.

- ~~The pad bindings have no wire surface at all~~ — `pad.bindings` and `pad.bind` are served
  over both transports. `robotd` answers them, not `configd`, which owns the rest of `pad.*`:
  checking a name against the skills this robot has is worth more than a tidy namespace, and
  routing is per method throughout anyway. §10.3 has the detail.

  `pad.bind` is the first call on either radio transport that **writes the config file**, and it
  has to be — `padd` re-reads `[pad]` every second, so a binding held in memory would be reverted
  before the caller let go of the phone. Which makes it, not `robot.loadPolicy`, the durable
  remote change: worth remembering when §4 of `remote-webrtc.md` is revisited.

- **A running skill has no fall reflex** for its whole duration (§10.2). Fine for a half-second
  kick; a skill configured to hold for ten seconds is ten seconds without one.
- **`robotctl configure` lists the skill table and cannot edit it.** A repeating table is not a
  key with a cursor position, so the editor points at `robotctl policy` instead. Teaching the
  registry about repeating sections is the largest single piece of that and buys the least.

- **Whether the official set ever becomes an updater component.** Delivery no longer needs one:
  §9 fetches the pinned set from the Hub directly, the way the board's other prerequisites
  arrive. So this is a question about what a component would *add* — rollback, pin, golden,
  known-bad history and the periodic check — against a cost that is not about policies at all.
  Every artifact the component path installs is signature-verified, unconditionally, by the same
  code that installs daemon binaries. An official set delivered that way must therefore be
  signed, not because a policy needs a signature but because that path has never had an unsigned
  mode and giving it one would widen the hole well past policies. Worth revisiting if per-set
  rollback turns out to be something anyone reaches for; nothing needs it today.
- Whether the app surfaces any of this, and how much of §7 it needs. Everything here is
  reachable over the same socket `btd` already relays, so the answer is a UI question rather
  than a protocol one.
- Whether the seven-session rebuild on every load is measurably bad on the board (§4). It is the
  status quo for mode switching, so this wants a measurement before any work.
