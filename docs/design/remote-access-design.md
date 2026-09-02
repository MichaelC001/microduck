# Remote access — an account, and a rendezvous behind it

Status: draft · Date: 2026-09-02 · Owner: pierre

How a duck is reached from outside the LAN. [`remote-webrtc.md`](remote-webrtc.md) §7 states the
shape — a bridge from a rendezvous service to the signalling server already running on the robot —
and it is right about the shape. This page owns the two things that shape needs and does not have:
a **credential that names an account**, and a **service to present it to**.

**Nothing here is built.** What is *established*, by probing the live services rather than by
reading about them:

- **Hugging Face implements the OAuth device grant.** Its discovery document advertises
  `device_authorization_endpoint: https://huggingface.co/oauth/device` and
  `urn:ietf:params:oauth:grant-type:device_code` among `grant_types_supported`. A full round trip
  short of the user's click works today: the endpoint returns a `user_code`, a `verification_uri` of
  `https://hf.co/oauth/device` and `expires_in: 300`, and polling `/oauth/token` answers
  `authorization_pending`. §2.2 has the transcript.
- **The client has to be public.** The device endpoint refuses the `reachy_mini` client id
  (`71146982-…`) with `invalid_client`: "if you want to use the device code flow without client
  secret authentication, delete the secret from the oauth app to make it public". §2.3.
- **The `reachy_mini` rendezvous Space is live and gated.** `pollen-robotics-reachy-mini-central.hf.space`
  serves a status dashboard anonymously; `/events` and `/api/robot-status` answer **401** without a
  token. Its *repository* is private, so nobody here can read the server. §4.
- **Its wire is not the gst signalling protocol on a WebSocket.** It is the same JSON envelopes over
  **HTTP** — SSE inbound, `POST` outbound — with per-hop peer and session ids. This corrects a claim
  in `remote-webrtc.md` §7; §3.2 says what it costs.

## 1. What has to be true for a duck to be reachable

Four things, and the robot has none of them:

1. a credential that names an account the robot belongs to (§2);
2. a service the robot reaches **outward**, which shows a robot only to its owner (§4);
3. a bridge from that service to `ws://127.0.0.1:8443` (§3);
4. a client that speaks the service's wire, served from somewhere the client can reach (§5).

Plus NAT traversal, which is a property of the media path rather than of any of the above (§6).

One invariant constrains all four, and it is not negotiable here: **local mode must not come to
depend on any of it.** `architecture.md`'s first invariant is that local recovery stays independent,
and `remote-webrtc.md` §7 extends it to media — if the service is down, a LAN client still connects.
Every choice below that could have been made more simply by routing local sessions through the
service too was made the other way for this reason, and §3.1 is where it costs something.

## 2. The account is an OAuth device flow against Hugging Face

### 2.1 Why the device grant, and not the flow `reachy_mini` runs

`reachy_mini` uses authorization code + PKCE with the redirect URI pointed back at the robot's own
HTTP server: `http://reachy-mini.local:8000/api/hf-auth/oauth/callback`, or `localhost` for the
tethered variant (`apps/sources/hf_auth.py`). It works, and three costs come with it:

- **A registered redirect URI per hostname.** The app has exactly one, which is why the mobile app
  carries a loopback HTTP bridge that catches HF's callback on `127.0.0.1:8000` and rewrites it as a
  302 onto a custom `reachymini://` scheme — a whole component whose stated purpose is avoiding an
  HF-side config change (`features/auth/oauthLoopback.ts`).
- **The browser must be able to resolve and reach the robot.** The callback is a URL *on the robot*.
  So logging in requires being on the robot's network, with mDNS working, which is the same class of
  problem `webrtc-console.md` §2 spends a section on for a page.
- **It is a browser flow on a device with no browser.** The robot is not the party that authenticates;
  it merely hosts the landing pad.

The device grant inverts that: the robot asks HF for a code, says *"open hf.co/oauth/device and type
M8HJ-FMGN"*, and polls until somebody has. No redirect URI, no hostname, no requirement that the
authorising device can reach the robot at all — a phone on cellular is fine. It is the flow specified
for a device with no browser and no keyboard, which is what a duck is.

The cost, stated plainly: somebody types eight characters. That is the whole of it, and it is smaller
than the mDNS dependency it removes.

### 2.2 The flow, and the transcript that proves it

```
POST https://huggingface.co/oauth/device
     client_id=<duck client>&scope=openid profile

  → {"device_code":"96e6e116-…","user_code":"M8HJ-FMGN",
     "verification_uri":"https://hf.co/oauth/device","expires_in":300}

POST https://huggingface.co/oauth/token
     grant_type=urn:ietf:params:oauth:grant-type:device_code
     &client_id=<duck client>&device_code=96e6e116-…

  → HTTP 400 {"error":"authorization_pending", …}   until the user approves
```

Three details the response fixes, and each is a decision we do not have to make:

- **No `verification_uri_complete`.** There is no QR-able URL that carries the code, so the code has
  to be *read by a person* — which means displaying it is the **client's** job, not the daemon's
  (§2.6). A robot with no screen cannot show it.
- **No `interval`.** RFC 8628's default of 5 s applies, and `slow_down` has to be honoured if it ever
  arrives.
- **`expires_in: 300`.** Five minutes, which is short enough that the client must show a countdown
  and long enough that nobody hurries.

### 2.3 The OAuth client is a decision — **open**

Three ways to get a `client_id`, and only one of them is a good idea:

- **Register one public app in the `pollen-robotics` org, no secret, device grant enabled.** One
  stable id baked into the release, revocable in one place, and the only thing on the robot is a
  public identifier. **Recommended.** It needs somebody with org admin to create it and hand back
  the id; that is the one thing in this page nobody here can do.
- **Reuse `reachy_mini`'s `71146982-…`.** Blocked, not merely inelegant: it is a confidential client,
  so the device endpoint demands Basic auth with its secret. Baking that secret into every duck's
  release makes it not a secret, and deleting it from the app to make it public would change the
  security posture of the mini's flow to suit ours.
- **Dynamic registration.** `POST https://huggingface.co/oauth/register` is unauthenticated and
  honours `token_endpoint_auth_method: "none"`, so a robot could mint its own public client at first
  login with no admin involvement at all. It works — that is how the transcript above was obtained.
  Rejected as the plan: a client per robot (or per release) is a fleet of identities nobody can
  enumerate or revoke, and it looks like abuse at scale. Worth knowing as the escape hatch if the org
  route is slow, since it means the design is not *blocked* on an admin.

### 2.4 Scopes: `openid profile read-repos`, and deliberately not more

The rendezvous needs to know *who* the token belongs to, which is `openid profile`. `read-repos` is
one scope further and buys something real: `policy install` reaching a **private** policy repo, from
the same token file, with no new mechanism (`policy-channel-design.md` §7).

`reachy_mini` asks for `openid profile read-repos write-repos manage-repos inference-api`, because
its robot installs apps and pushes artifacts. **Do not copy that list.** A duck holds this token
unattended, on a board that can be lost or given away; a credential that can *push* to the owner's
repositories is a much worse thing to lose than one that can read them. If a future feature needs to
write, it is a scope change with a re-login, which is a day's work — where a robot that has been able
to write all along is not recoverable.

### 2.5 Where the token lives, and who writes it

`/etc/robot/hf-token`, `root:robot`, `0640`.

**Not in `robotd.toml`.** Every mechanism that exists for that file is wrong for a secret:
`robotctl configure --list` prints what a robot changes, `policy-channel-design.md`'s full-screen
editor shows the file, and "what has been changed on this robot" is a report we now generate. A
bearer token would be in all three outputs. Its own file, with its own mode, is the whole of the
protection it gets — and §7 says why that is enough.

**`updaterd` owns it.** It already has the HTTP client, already reaches the network on the robot's
behalf, already runs as `root`, and already has a namespace of calls that write system state
(`policy.*`). `configd` owns *config*, has no HTTP client, and adding outward network egress to the
daemon that answers `system.info` would be a new kind of thing for it. `mediad` must not own it: it
runs as `User=mediad` under `ProtectHome=yes`, and it is the process a remote peer talks to.

**`mediad` reads it** — on each connect attempt, plus a slow poll (30 s) while it has none, which is
also its `waiting for token` state. No cross-daemon notification: `reachy_mini` has a
`notify_token_change` call from its auth router into its relay, and re-reading the file on the
reconnect that is going to happen anyway makes it unnecessary. The cost is that a fresh login takes
up to a poll interval to become a live producer, which nobody can perceive.

### 2.6 The calls, and which transports may reach them

`account.login`, `account.status`, `account.logout` on `updaterd`. `login` starts the flow and returns
`verification_uri`, `user_code` and `expires_in`; progress is a notification, the way `update.*`
already pushes progress, so a client that reconnects can resubscribe (`duck-ipc-proto`'s rule).

**`account.login` is the highest-authority call on the robot, and it is worth saying why.** Whoever
completes it binds the robot to *their* Hugging Face account and thereby gets remote access to its
camera and its control channel from anywhere. Nothing else in the API hands the robot to a stranger;
`robot.shutdown` merely stops it.

So it is **local and BLE only, refused over WebRTC** — the same table `route.rs` already keeps, with
the same reasoning `system.setPairingPin` is refused by: BLE is the authenticated transport, and ten
metres of radio means whoever ran it is in the room (`remote-webrtc.md` §4). `account.status` is safe
everywhere and useful everywhere — a client wants to say "signed in as *someone*, relay connected".
`account.logout` moves the same boundary as `login` and goes with it.

### 2.7 Whether the token expires is the one unknown that matters — **open**

HF advertises `refresh_token` in `grant_types_supported`, and what the token response actually carries
— `expires_in`, `refresh_token`, or neither — cannot be known without one real authorization, which
needs a human click. It decides a real piece of work:

- **No expiry** → store the access token, and §2.5 is finished.
- **Expiry** → store the refresh token too, refresh ahead of time, and handle a refresh that fails as
  "signed out" rather than as an error nobody sees. That is a scheduler and a second secret.

`reachy_mini` stores only the access token, at `~/.cache/huggingface/token`, and its robots stay
reachable — evidence that long-lived tokens are at least possible, not proof about the device grant's.
**Answer this before writing §2.5's storage**, because "one string in a file" and "two strings plus a
clock" are different designs and the cheap one is only correct if the answer is no.

## 3. The bridge

### 3.1 A listener on loopback, not a signaller inside `webrtcsink`

`webrtcsink` takes a custom signaller (the `Signallable` interface), so the temptation is to implement
one that speaks the rendezvous wire directly: one hop fewer, no id translation, no local WebSocket
client. **Rejected**, for a reason that is structural rather than aesthetic: one `webrtcsink` has one
signaller. Pointing it at the service means local sessions go through the service too — which breaks
§1's invariant — and keeping both means a second `webrtcsink` off the tee, which means encoding the
same frames twice on a board where the encoder is the budget.

So the bridge is what `remote-webrtc.md` §7 describes, and what `reachy_mini` runs:

```
  rendezvous  ──SSE──►  relay task  ──ws──►  127.0.0.1:8443  ◄──ws──  webrtcsink
  (HTTP)      ◄─POST──  (in mediad)  ◄──ws──  signalling server      (the producer)
```

The relay registers with the service as a **`producer`** and with the local server as a
**`listener`** — the roles are inverted on the two sides, because to the service it *is* the robot and
to the pipeline it is a peer asking for a session.

### 3.2 What it translates, and the correction to §7

§7 says "the bridge parses nothing. It proxies the gst signalling protocol, which is the same protocol
a LAN client speaks." The payloads — SDP and ICE — are indeed opaque and stay that way. The envelope
is not, in three ways:

| | local side | rendezvous side |
|---|---|---|
| transport | WebSocket | SSE inbound, `POST /send` outbound |
| auth | none (§4 of `remote-webrtc.md`) | `Authorization: Bearer <hf token>` |
| ids | its own `peerId`, its own `sessionId` | different ones, per hop |
| our role | `listener` | `producer` |

So the bridge keeps a session table both ways and rewrites `sessionId` on every `peer` message. That
is where `reachy_mini`'s relay has needed most of its scar tissue (§3.4), and it is the honest
description: **a translator with an opaque payload**, not a relay. The payoff §7 claims for
`webrtcsink` over `webrtcbin` survives — the protocol still exists rather than being invented, and the
translation is a table rather than a parser — but "proxies, parses nothing" should stop being said.

### 3.3 The lease, and why the heartbeat is not optional

The service evicts a producer that has sent nothing for `LEASE_SECONDS`, whatever its socket looks
like. That is not defensive over-engineering on its part: a half-open TCP connection — wifi yanked,
NAT rebinding, a sleeping captive portal — absorbs server-pushed keepalives silently for minutes,
during which the robot believes it is reachable and is not.

So the relay re-emits `setPeerStatus` periodically, at a cadence **negotiated from the welcome**:
`recommended_heartbeat_interval_seconds` if offered, else `lease_seconds / 3`, else 5 s — clamped to
[1 s, 60 s] so a misconfigured service can neither ask for a request storm nor talk us into a cadence
slower than our own eviction. That ladder is `reachy_mini`'s and it is right; the clamp is the part
worth copying most.

### 3.4 The failure modes are already known, which is the main reason to read their relay

Four, each cheap to build in now and expensive to rediscover:

- **Split-brain.** The SSE stream is healthy and the service no longer lists us — a `setPeerStatus`
  round trip cancelled mid-flight leaves exactly this. Nothing in the connection notices. Their
  answer: poll `/api/robot-status` every 30 s, and force a reconnect after two consecutive misses.
- **Concurrent sessions.** The service is supposed to gate them; enforce one at a time on the robot
  anyway, and refuse with an `endSession` carrying a *reason* rather than by silence. A second peer
  driving the same robot is `remote-webrtc.md` §9's interleaving bug with two remote writers instead
  of a pad and a peer.
- **Ordering at registration.** Register as a producer *before* reporting `connected`, or every
  observer — a status call, a page, a person — sees "remote access enabled" while the service does not
  yet know the robot exists.
- **Backoff with jitter, capped.** 5 s growing to 60 s, plus ~10%. A fleet reconnecting in lockstep
  after a service restart is a self-inflicted outage.

What we do **not** copy is their `RobotAppLock`: it arbitrates a local *app* against a remote session,
and a duck has no app. `remote-webrtc.md` §9 owns the equivalent question here (a pad and a peer both
writing intents) and defers it deliberately; a second remote peer is the same gap, not a new one.

### 3.5 It is a task in `mediad`, in a module with no GStreamer in it

Not a new unit. A `relayd` would need its own copy of the producer identity, its own config, its own
restart story, and it would still be useless without `mediad` running — three new moving parts to
isolate a task that is a websocket, an HTTP client and a hash map.

It goes where `session.rs` went, and for the same reason: **transport-agnostic on purpose**, so it is
testable on a laptop against a fake service and a fake local server. That is what made the control
channel testable without a board and it is worth twice as much here, because every failure in §3.4 is
a timing failure that no manual test on hardware will reproduce on demand.

One inherited rule: nothing in a GStreamer signal handler may panic (`pipeline.rs`'s header, and the
process abort that taught it). The relay never touches one — but it will want to *reach* the pipeline
eventually, and that is the boundary to keep clean.

### 3.6 What a bridged peer may call: exactly what a LAN peer may

The session is the same `webrtcsink` session and `route.rs` is the same table. This is deliberate and
it is also the part worth re-examining once it works: §4 argues the robot needs no gate because the
service authenticated both ends, and after this page that argument gets *stronger* — a bridged peer
has proved account ownership, where a LAN peer has proved only that it is on the wifi. The robot can
tell them apart by source address (§7 notes it), and nothing yet acts on the difference. Keep it true.

## 4. Which rendezvous — **open**

`pollen-robotics-reachy-mini-central.hf.space` is live, and what a robot needs from it is small:
`GET /events` (SSE, Bearer), `POST /send` (Bearer), `GET /api/robot-status`. Its `meta` is free-form,
so a duck registers with whatever identifies it — `producer.rs` already assembles exactly the fields a
listing wants (name, serial, release, `api_version`), which is what `webrtc-console.md` §5 predicted
the rendezvous would need.

**Reusing it** costs no backend work, is proven under a fleet of real robots, and makes the whole of
this page a client-side project. Against that: we do not own its deploys, its lease and session
gating were written around a robot with a different lock model, a duck appears in whatever lists a
user's robots (a feature or a bug depending on intent), and **its repository is private** — so nobody
on this side can read the server they depend on, which is how §3.2's protocol surprise happened at all.

**Our own instance** is a Space and a couple of hundred lines: we own the lease policy, the eviction
sweeper and the deploy schedule, and the duck's identity model is ours. Against that: a second service
to run and watch, duplicating something that exists and works.

**Recommendation: reuse for the first proof, and decide on the answer to §5.** The service URL is one
config key — `reachy_mini` keeps it in an env var for exactly this reason — so moving later costs a
line. What actually couples us is not the relay, it is the client: if the client is a page we publish,
the service stays a rendezvous and reuse is nearly free; if the client is somebody else's app, we are
inside their product and should own neither the Space nor the decision alone.

## 5. The client, and where it is served — **open**

The console is `include_str!`'d into `mediad` and served by the robot (`webrtc-console.md` §1), which
works because the client is on the LAN. **A remote client cannot fetch a page from a robot it cannot
reach**, so remote needs the page hosted off-robot *and* a second signalling transport in it.

Three ways:

- **The same file, published by CI to GitHub Pages**, with the transport chosen by the URL it is given.
  One page, two hosts, still no build step — the constraint `webrtc-console.md` §8 defends survives.
  The service stays a rendezvous and nothing about the client lives in it.
- **The service serves the page.** One host and one deploy, at the cost of putting the client inside a
  service we may not own; a page in a private repo is a page nobody here can edit.
- **No client yet.** Prove login and the relay with the service's own dashboard, `/api/robot-status`
  and `duckctl` — which needs no page at all, and is the whole of slices 1 and 2 in §8.

**Recommendation: the third, then the first.** The proof that the token path and the lease work needs
no UI, and deferring the hosting decision by a slice costs nothing.

One thing to know before that page is written: `EventSource` **cannot set headers**, so a browser
speaking the service's SSE wire must either put the token in the query string — which is what
`reachy-mini-js` does, and what puts a bearer token into Space access logs and every proxy in between
— or read the stream with `fetch` and parse SSE by hand. The robot relay uses a header and is right to
(`central_signaling_relay.py` says so in as many words). The page should too, which makes it `fetch`
plus a few lines of line-splitting rather than one browser API.

## 6. NAT: decide the STUN server, defer TURN

`webrtcsink` defaults its `stun-server` to a public Google address. LAN sessions need none, so nothing
has exercised it — and the moment remote works, **a duck's reachability quietly depends on a third
party we do not run**. Set the property rather than inherit it, for the same reason
`remote-webrtc.md` §0 sets `congestion-control` to the value that is already the default: the day
upstream changes it should not be the day every robot's connectivity changes with it.

TURN is what makes symmetric NAT and CGNAT work at all, and it relays the *whole* session's media at
somebody's expense. Not in the first slice, and `remote-webrtc.md` §11 is right that the decision
belongs with whoever runs the rendezvous rather than with the daemon.

## 7. Authorisation, restated now that there is an account

§4 of `remote-webrtc.md` argues the robot needs no gate of its own because a bridged session was
authenticated twice before it arrived — the client to the service, the robot outward with a token —
and that the trust therefore *moved* into the service rather than vanishing. This page does not change
that argument; it adds the one thing §4 could not name, which is **when the binding happens and who
performs it**:

- Before `account.login`, a duck is unreachable from outside the LAN. There is nothing to attack.
- After it, one account owns it, and `account.login`/`account.logout` are the calls that can move that
  ownership — hence BLE-and-local only (§2.6). A remote peer that could re-bind the robot to its own
  account would be the one call able to take the robot away from its owner.
- A robot that changes hands must be logged out. That is the same list as the pairing PIN and the
  calibration — a hand-over process, in M6 — and this is one more item on it, worth adding while the
  list is still being written rather than after a second-hand duck streams to a stranger.
- The token is a bearer credential in a file, so a stolen board yields it. The answer is §2.4's
  read-only scopes, not encryption: a robot has to read this file unattended at boot, so anything it
  can decrypt without a human is something the thief can decrypt too.

## 8. Order of work

Five slices, and the first two are independently useful and need no client:

1. **`account login`** — the device flow, the token file, `account status`. `updaterd`. Verifiable on
   its own: it prints the Hugging Face username. Answers §2.7 the moment it first succeeds.
2. **The relay, registering only** — producer registration, the negotiated heartbeat, reconnect and
   backoff, the split-brain poll. `mediad`. Verifiable with no client at all: the service's dashboard
   counts a producer and `/api/robot-status` lists the duck.
3. **Session translation** — a remote consumer gets video and the `control` channel. The first slice
   that needs something to connect *with*.
4. **The client, hosted.** §5.
5. **STUN decided; TURN if a real network needs it.** §6.

## 9. What is open, and who can close it

| | needs |
|---|---|
| §2.3 the OAuth client id | one public app in the `pollen-robotics` HF org, no secret, device grant — created by somebody with org admin |
| §2.7 token expiry | one real authorization, then read the token response. A click |
| §4 which rendezvous | read access to the Space's repository, **or** the decision to stand up our own |
| §5 where the client is served | follows §4, and is the decision that actually couples us to a service |

## 10. Not doing

- **The `teleop` datachannel.** `remote-webrtc.md` §6 owns it; a remote session makes head-of-line
  blocking more visible, not more urgent.
- **`update.*` mutations over a remote session.** §8 of `remote-webrtc.md` says what it will take, and
  the answer is a client that survives the restart rather than anything here.
- **Multi-peer.** One media session at a time, as before.
- **Per-session consent.** An M5 item, orthogonal to this page and made more pointed by it: a stream
  that can be started from another continent is the case `architecture.md` §7 was written for.
- **A duck-specific mobile app.** #107 designs one and M6 owns the phone spike. This page's client
  question (§5) is deliberately answerable without it.
