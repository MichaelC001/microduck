# The Policy Channel — policies from the Hub

Status: draft · Date: 2026-08-31 · Owner: pierre

Where the ONNX policies come from, how someone tries one they did not train, and what
"reset" puts back.

**Built so far** (2026-08-31): everything §3, §4, §5 and §9's runtime half, plus the local part
of §7 — `robotctl policy list`, `load` and `reset`, over `robot.loadPolicy` and `robot.policies`.
A policy on the board can be tried and undone without editing a file or restarting anything, and
`robotd` reads its policies from `/opt/robot/policies/current`, outside the release.

**Not yet true**: `pollen-robotics/microduck-policies` does not exist yet, so every board is
still on the transitional fallback in §9 — the copy the release carries. Creating that repo and
tagging `v1` is what switches the fetch on; deleting `policies/` is what finishes §9. There is no
library, no provenance sidecar, and no `check`, `update` or `search`, so `origin` is only ever
`official` or `local`. See [`roadmap.md`](../project/roadmap.md) §M8.

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
loop            → finish the tick, home the robot with torque on
at home         → re-resolve the params, rebuild the controller (load + warm-up)
on success      → publish the new policy names, resume
```

Two differences from a mode switch, and only the second needs new code.

**Scope.** A load overrides one field of `policy_params` and re-resolves, exactly as the mode
switch sets `policy_params.mode` and re-resolves. Rebuilding the controller reloads all seven
sessions rather than the one that changed. That is the cost the mode switch already pays and
which has been accepted in shipped behaviour, so it is not being optimised here; if it turns out
to matter on the board, loading on a worker thread and swapping in at the home pose is a fix for
both callers and should be done as one.

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
because a script that runs from inside a release cannot read the manifest. **Bumping the pin is
the whole of shipping a new gait**: no daemon release, no restart, and a board takes it at its
next update.

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

One rule makes that a bootstrap rather than a second home: **it never touches a set it did not
install**. A `current` pointing anywhere but a `seed-*` directory means something else put
policies there, and the release stops overwriting them for good. No flag, no config, and nothing
to remember at handover — whatever ends up publishing bundles just installs one, and the seeding
is over. A *newer* seed does replace an older one, because while the release is the only source
of policies a daemon update is still how a retrained gait reaches a board.

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

**One transitional branch remains.** The release still carries its copy of the set, and a board
with *nothing* installed that cannot reach the Hub falls back on it, so a first install without a
network gets a gait rather than none. That branch — and `policies/`, and the `--include` lines in
the two release workflows and `scripts/dev-push.sh`, and `xtask`'s
`every_policy_in_the_repo_is_packaged` — goes as soon as the Hub repo is populated and the fetch
is proved on a board. It exists so that proving it cannot leave a duck that will not walk.

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

## 10. Naming

The updater says `model`; `robotd` says `policy`. Standardising on **policy** — the component is
`policies`, the namespace is `policy.*`, the commands are `robotctl policy …`. `models/` keeps
its meaning for the things that genuinely are models and not control policies, such as
`pet_detect.onnx` and the duck detector.

## 11. Decisions recorded

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
| Seeds are pruned to the current and previous | Unbounded 7 MB-per-release growth; the previous one is the hand-recovery after a rollback (§9) |
| One `policy check`, routing internally | Two commands for one question is the confusion worth avoiding (§6) |

## 12. Deferred, deliberately

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

## 13. Open

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
