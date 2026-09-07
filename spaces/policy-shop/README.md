---
title: microduck policy shop
emoji: 🦆
colorFrom: yellow
colorTo: pink
sdk: gradio
app_file: app.py
pinned: false
hf_oauth: true
short_description: Pick a policy off the Hub, put it on your duck, run it.
---

# microduck policy shop

Everything published to the Hub as `microduck-…`, read the way the robot reads it, with a button
that downloads one onto your duck and runs it.

**Do not edit this Space directly.** The source is `spaces/policy-shop/` in
`pollen-robotics/microduck`, and `scripts/publish-space.sh policy-shop` is what puts it here.

## The one click

Four calls over the session's `control` channel, in the order `robotctl policy add` makes them:

| | |
| --- | --- |
| `policy.fetch {repo, file}` | `updaterd` downloads it and reads the `manifest.json` beside it |
| `robot.setSkill {name, path, duration, …}` | `robotd` writes the entry and re-reads its skills |
| `robot.policies` | did the reload take? `change_error` is where it says no |
| `robot.do {skill}` | run it |

Nothing is invented on either side: those are `duck-ipc-proto` method names, already in
`mediad/src/route.rs`'s permitted set, and `mediad` knows none of them — the control channel is a
pipe to the API the robot already serves.

**The robot's reading of a manifest wins over this page's.** The catalogue is read here so a row
can say what a policy claims to be before anybody clicks it, but the skill that gets written comes
from `policy.fetch`'s answer, which is about the bytes that were actually downloaded.

## What it refuses, and why it is this side's job

A policy whose command the daemon generates — a phase for a ground pick, a flag for a sit↔stand —
cannot be a one-shot skill. `robot.setSkill` would accept the entry, and the robot would then feed
a constant to a network trained on a phase: it moves plausibly and wrongly, which is worse than a
refusal. `robotctl`'s `skill_encoding_refusal` is the rule, `robotctl` is not in the path of a
click, so `catalogue.refusal` is the same rule again.

The shape claims — `obs_len`, `action_len`, `model_api`, `robot.model` — are deliberately *not*
repeated here. `policy.fetch` checks them itself, before the download, so "this policy is 51-D and
this robot is 61-D" comes back in a second and comes back from the robot.

A `perpetual` policy has no length of its own, so a one-shot made out of one is a hold and an
unwind. `robotctl policy add` refuses without `--hold` rather than picking a number; this page asks
instead, and that is the seconds box above the rows.

## Two things that will bite before the robot does

**Media may not connect from a data centre, and that is not a fault here.** A relay candidate needs
`turn.fastrtc.org`, which has no A record and whose zone has no NS records at all
(`remote-access-design.md` §6), so a session falls back to host and srflx — often enough to punch
a hole between a home router and a container, and often enough not. The control channel is SCTP
over that same candidate pair, so when it does not punch, nothing here works and the status line
says which stage was reached rather than "connecting…". Running this file on a laptop on the
robot's own network is the way through:

```bash
HF_TOKEN=hf_… python app.py
```

**One consumer at a time.** The rendezvous's rule, not a simplification: while this Space holds a
session, the robot's own console cannot open one, and neither can the vision demo.

## Identity

**A visitor's token by preference, and never the robot's.** The rendezvous maps a token to one
peer, so a consumer authenticating as the robot would take the robot off its owner's listing.
`hf_oauth: true` plus Gradio's login button gives each visitor their own, which reaches their own
robots and nobody else's — and is what makes a public Space defensible. `HF_TOKEN` is the fallback
for a private Space with one owner, and for the laptop run above.

Ducks are told from minis on `meta.kind`, and the chosen robot is pinned by `peerId` rather than by
name: their consumer's auto-pick falls back to the only visible producer whatever it is called, so
an account with one duck and one mini could otherwise hand this page a mini to drive with method
names it does not serve. §5.1 has the other half, which is theirs.

## Checking it without a robot

`python catalogue.py` prints what a duck would be offered — every policy on the Hub, what it
claims, which ones are refused and why. That is the half of this Space that needs no token, no
session and no hardware.
