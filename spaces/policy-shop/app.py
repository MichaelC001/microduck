"""Pick a policy off the Hub, put it on your duck, and watch it run — from anywhere.

The robot is behind somebody's router and this is a container in a data centre; what joins them is
the rendezvous service the duck registers with (`docs/design/remote-access-design.md` §3). The
robot is a *producer*, this is a *consumer*, and the WebRTC session between them carries the
camera and one data channel called `control` — JSON-RPC 2.0, `duck-ipc-proto`'s own wire, the same
lines `robotctl` sends over a unix socket.

## What the one click is

Four calls, in the order `robotctl policy add` makes them, and nothing invented:

    policy.fetch    {repo, file}   → updaterd downloads it and reads the manifest beside it
    robot.setSkill  {name, path…}  → robotd writes the entry and re-reads its skills
    robot.policies                 → did the reload take? `change_error` is where it says no
    robot.do        {skill}        → run it

Every one of them is in `mediad/src/route.rs`'s permitted set already, and the pipe is dumb: no
method here is known to `mediad`, which is what `remote-webrtc.md` §5 means by the control channel
being a pipe to the existing API.

## Two things that will bite before the robot does

**Media may not connect from a data centre, and it is not this Space's fault.** A relay candidate
needs `turn.fastrtc.org`, which has no DNS at all right now (§6), so the session falls back to
host and srflx — often enough to punch a hole, and often enough not. The control channel is SCTP
over that same candidate pair, so when it does not punch, nothing here works. Running this file on
a laptop on the robot's own network is the way through: `HF_TOKEN=… python app.py`.

**One consumer at a time.** That is the rendezvous's rule, not a simplification here: while this
Space holds a session, the robot's own console cannot open one, and the vision demo cannot either.
"""

from __future__ import annotations

import asyncio
import os
import threading
import time
from dataclasses import dataclass
from typing import Any

import gradio as gr
import requests
from reachy_mini.media.central_consumer import DEFAULT_CENTRAL_URL

import catalogue
from catalogue import Policy
from control import DuckConsumer, Rpc, RpcError

# `meta.kind`, which is on the wire so that one client can list two families of robot without
# opening a session to ask what it found. Filtering on it is §5.1's whole point: their clients
# select on `meta.name` with a fallback to "the only producer visible", so an account with one
# duck and one mini can hand either side the wrong robot. This one filters, and then pins the
# peer id rather than the name.
DUCK = "microduck"

# How long to hold a policy that declares no length of its own. Perpetual means "until told
# otherwise", so something has to choose, and `robotctl policy add` refuses rather than guessing —
# a page can ask instead, which is what the number beside the button is.
DEFAULT_HOLD = 3.0

# Rows drawn for the catalogue. Gradio wants its components at build time, so this is a ceiling
# rather than a count: 23 policies were published when this was written.
MAX_ROWS = 40

# The download happens on the robot, over the robot's wifi. Everything else here is a question
# about state and answers in milliseconds.
FETCH_TIMEOUT = 180.0

CATALOGUE: list[Policy] = []
CATALOGUE_ERROR: str | None = None
CATALOGUE_READ_AT: float = 0.0
# How long a peer id stays worth a name in the status line. Filled by the listing.
NAMES: dict[str, str] = {}


@dataclass
class Link:
    """The one session this Space holds, and the thread that owns its event loop.

    A dedicated loop in a background thread rather than Gradio's: the connection outlives any one
    request, `aiortc` wants a single loop for the life of a peer connection, and every send is
    marshalled back onto it.
    """

    consumer: DuckConsumer | None = None
    loop: asyncio.AbstractEventLoop | None = None
    rpc: Rpc | None = None
    robot: str | None = None
    error: str | None = None
    started_at: float | None = None

    def __post_init__(self) -> None:
        self.lock = threading.Lock()

    # ── the listing ──────────────────────────────────────────────────────────

    @staticmethod
    def robots(token: str) -> tuple[list[tuple[str, str]], str]:
        """The ducks this token can reach, and a line about what else was there.

        `/api/robot-status` rather than the SSE `list` push, for the reason their consumer gives:
        `list` only re-fires on a producer *status change*, so a robot that was already online
        before anybody connected can be absent from the stream entirely.
        """
        try:
            answer = requests.get(
                f"{DEFAULT_CENTRAL_URL}/api/robot-status",
                headers={"Authorization": f"Bearer {token}"},
                timeout=15,
            )
        except requests.RequestException as e:
            return [], f"the rendezvous did not answer: {type(e).__name__}: {e}"
        if answer.status_code == 401:
            return [], "the rendezvous refused this token. Sign in again."
        if answer.status_code != 200:
            return [], f"the rendezvous answered {answer.status_code}."
        try:
            robots = answer.json().get("robots") or []
        except ValueError:
            return [], "the rendezvous answered something that is not JSON."

        ducks, others = [], []
        for robot in robots:
            meta = robot.get("meta") or {}
            name = meta.get("name") or robot.get("robotName") or "a robot with no name"
            peer = robot.get("peerId") or robot.get("id")
            if not peer:
                continue
            if meta.get("kind") == DUCK:
                release = meta.get("release") or "release unknown"
                ducks.append((f"{name} — {release}", peer))
            else:
                others.append(f"{name} ({meta.get('kind') or 'kind not declared'})")

        if not ducks and others:
            return [], (
                f"{len(robots)} robot(s) on this account and none of them a duck: "
                + ", ".join(others)
                + ". A duck registers with `kind: microduck`."
            )
        if not ducks:
            return [], (
                "no robots online for this account. A duck registers when it has been signed in "
                "(`robotctl account login`) and has a network — `duckctl account status` says "
                "which of the two is missing."
            )
        note = f"{len(ducks)} duck(s)"
        if others:
            note += f", and {len(others)} other robot(s) not listed here"
        return ducks, note

    # ── the session ──────────────────────────────────────────────────────────

    def connect(self, token: str, peer_id: str, label: str) -> str:
        with self.lock:
            if self.consumer is not None:
                return f"already connected to {self.robot}"

            self.error = None
            rpc = Rpc()
            loop = asyncio.new_event_loop()
            thread = threading.Thread(target=loop.run_forever, name="duck-consumer", daemon=True)
            thread.start()

            consumer = DuckConsumer(
                hf_token=token,
                # **Pinned, not matched by name.** Their auto-pick falls back to the only visible
                # producer whatever it is called, so a name that does not match can still hand
                # this Space somebody's mini. The listing above already knows the id.
                robot_peer_id=peer_id,
                consumer_label=f"microduck-policy-shop/{os.environ.get('SPACE_ID', 'local')}",
                rpc=rpc,
            )
            try:
                asyncio.run_coroutine_threadsafe(consumer.start(), loop).result(timeout=30)
            except Exception as e:  # noqa: BLE001 - reported, never raised into a UI callback
                self.error = f"{type(e).__name__}: {e}"
                loop.call_soon_threadsafe(loop.stop)
                return f"could not start: {self.error}"

            self.consumer, self.loop, self.rpc = consumer, loop, rpc
            self.robot, self.started_at = label, time.monotonic()

        # Blocking here rather than leaving the page to poll: nothing below the buttons can be
        # answered until the channel is open, and a page that says "connected" while every call
        # fails is the worst of the three states.
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if rpc.is_open():
                return f"connected to {label}, control channel open"
            time.sleep(0.5)
        return self.describe()

    def disconnect(self) -> str:
        with self.lock:
            consumer, loop, rpc = self.consumer, self.loop, self.rpc
            self.consumer, self.loop, self.rpc = None, None, None
            self.robot, self.started_at = None, None
        if rpc is not None:
            rpc.abandon("disconnected")
        if consumer is None or loop is None:
            return "not connected"
        try:
            asyncio.run_coroutine_threadsafe(consumer.stop(), loop).result(timeout=10)
        except Exception as e:  # noqa: BLE001 - a teardown that fails still ends the session
            return f"disconnected, with a complaint: {type(e).__name__}: {e}"
        finally:
            loop.call_soon_threadsafe(loop.stop)
        return "disconnected"

    def call(self, method: str, params: dict[str, Any] | None = None, timeout: float | None = None) -> Any:
        rpc = self.rpc
        if rpc is None:
            raise RpcError(method, {"message": "not connected"})
        return rpc.call(method, params, timeout)

    def describe(self) -> str:
        """Which stage the connection reached, because the stages fail differently.

        A blank "connecting…" is the worst thing this panel can say: signalling failing, ICE
        failing and a data channel that never opened look identical from the outside and have
        nothing in common.
        """
        consumer, rpc = self.consumer, self.rpc
        if consumer is None:
            return f"**not connected** — {self.error}" if self.error else "**not connected.**"

        status = consumer.status()
        session, state = status.get("session_id"), status.get("pc_state")
        waited = time.monotonic() - (self.started_at or time.monotonic())

        if not session:
            return (
                f"**no session yet** after {waited:.0f}s. The rendezvous has not paired this "
                "consumer with the robot: either it went offline, or another consumer holds it — "
                "one at a time, and the robot's own console counts."
            )
        if state != "connected":
            return (
                f"**signalling worked and the peer connection has not** — session "
                f"`{session[:8]}`, state `{state}` after {waited:.0f}s. This is the case a data "
                "centre is expected to hit: neither side offers a `relay` candidate, because the "
                "TURN credentials endpoint has no DNS (§6). Running this file on the robot's own "
                "network is the way through."
            )
        if rpc is None or not rpc.is_open():
            return (
                f"**connected, and no control channel** — session `{session[:8]}`, "
                f"{status.get('frames') or 0} frames. Media crossed and `control` did not open, "
                "which is a duck running a daemon that opens no data channel."
            )
        return (
            f"**connected to {self.robot}** — session `{session[:8]}`, "
            f"{status.get('frames') or 0} frames, control channel open."
        )

    def transcript(self) -> str:
        rpc = self.rpc
        if rpc is None or not rpc.transcript:
            return ""
        return "```\n" + "\n".join(rpc.transcript) + "\n```"


LINK = Link()


# ── what the robot already has ───────────────────────────────────────────────


def robot_state() -> tuple[str, list[str]]:
    """What is loaded and what it can be asked to do, as one read.

    `robot.policies` carries the skills as well as the slots — deliberately, because a client
    cannot offer a "bow" button without knowing the robot has a bow, and the other place that
    list is published is a 50 Hz stream.
    """
    try:
        policies = LINK.call("robot.policies") or {}
        skills = LINK.call("robot.skills") or {}
    except RpcError as e:
        return f"could not read this robot: {e.message}", []

    lines = [f"**mode** `{policies.get('mode') or '?'}`"]
    if not policies.get("enabled"):
        lines.append(
            "**policies are switched off on this robot** (`[policy] enabled`), so nothing below "
            "will move it. A legitimate bench configuration, not a fault."
        )
    if policies.get("change_error"):
        lines.append(f"**last change failed:** {policies['change_error']}")

    slots = policies.get("slots") or []
    if slots:
        lines.append("")
        lines.append("| slot | running | origin |")
        lines.append("| --- | --- | --- |")
        for slot in slots:
            # `path` is what is *actually* loaded, which is the point of the call: a slot
            # whose override failed reports the file it fell back to and says why.
            path = slot.get("path") or "*empty*"
            warn = f" — ⚠️ {slot['error']}" if slot.get("error") else ""
            lines.append(
                f"| `{slot.get('slot')}` | {path}{warn} | {slot.get('origin') or '–'} |"
            )

    table = skills.get("skills") or []
    names = [s.get("name") for s in table if s.get("name")]
    built_in = skills.get("built_in") or []
    if table:
        lines.append("")
        lines.append("| skill | seconds | from config |")
        lines.append("| --- | --- | --- |")
        for skill in table:
            seconds = skill.get("duration")
            lines.append(
                f"| `{skill.get('name')}` | {seconds if seconds is not None else '–'} | "
                f"{'yes' if skill.get('overridden') else 'shipped'} |"
            )
    if built_in:
        lines.append("")
        lines.append(
            "The daemon drives these itself, so they are not editable here: "
            + ", ".join(f"`{name}`" for name in built_in)
        )
    return "\n".join(lines), names + [n for n in built_in if n not in names]


def not_accepted(result: Any) -> str | None:
    """Why a discrete intent said no, or `None`.

    **`accepted: false` is a normal answer and not a JSON-RPC error**, which `IntentResult` says
    outright: safety may refuse to run a policy on a fallen robot, and the caller needs the reason
    rather than something that reads as "the call broke". A page that only caught errors would
    report every one of those as a success and leave a motionless robot unexplained.

    An `accepted: true` carrying a reason is `IntentResult::already` — it succeeded and queued no
    work — so it is not a refusal and is worth saying anyway, which is `already_note`.
    """
    if not isinstance(result, dict) or "accepted" not in result:
        return None
    if result.get("accepted"):
        return None
    return result.get("reason") or "refused, with nothing said about why"


def already_note(result: Any) -> str:
    """An acceptance's reason, which means the robot was already in the state asked for."""
    if isinstance(result, dict) and result.get("accepted") and result.get("reason"):
        return f" — {result['reason']}"
    return ""


# ── the one click ────────────────────────────────────────────────────────────


def install_and_run(index: int, hold: float) -> str:
    """`policy.fetch`, `robot.setSkill`, `robot.policies`, `robot.do` — and stop at the first no.

    Every refusal is worth showing verbatim. `policy.fetch` is the one that checks the claims that
    matter — `obs_len`, `action_len`, `model_api`, `robot.model` — and it makes them *before* the
    download, so "this policy is 51-D and this robot is 61-D" arrives in a second rather than
    after 800 KB and a load failure.
    """
    if index >= len(CATALOGUE):
        return "that row is stale — reload the catalogue."
    policy = CATALOGUE[index]

    blocked = catalogue.refusal(policy)
    if blocked:
        return f"**not installed.** {blocked}"

    params: dict[str, Any] = {"repo": policy.repo}
    if policy.file:
        params["file"] = policy.file

    try:
        fetched = LINK.call("policy.fetch", params, timeout=FETCH_TIMEOUT) or {}
    except RpcError as e:
        return f"**`policy.fetch` refused it.** {e.message}"

    # The robot's own reading of the manifest wins over this Space's: it downloaded the file and
    # parsed the manifest beside it, so its answer is about the bytes that are going to run.
    if not fetched.get("duration_s") and not hold:
        return (
            f"**{policy.name} holds until it is told otherwise**, so it has no length of its own. "
            "Say how many seconds to hold it, beside the button."
        )
    late_refusal = catalogue.refusal(
        Policy(repo=policy.repo, name=policy.name, encoding=fetched.get("encoding"))
    )
    if late_refusal:
        return f"**downloaded, and not installed.** {late_refusal}"

    skill = catalogue.skill_for(fetched, hold)
    try:
        added = LINK.call("robot.setSkill", skill)
    except RpcError as e:
        return f"**downloaded to `{fetched.get('path')}`, and `robot.setSkill` refused.** {e.message}"
    said_no = not_accepted(added)
    if said_no:
        return f"**downloaded to `{fetched.get('path')}`, and not added:** {said_no}"

    # A skill accepted is not a skill the robot has: `robot.setSkill` triggers a reload, and a
    # reload that failed says so here and nowhere else. Without this check the page would report
    # success and the robot would do nothing.
    try:
        after = LINK.call("robot.policies") or {}
    except RpcError as e:
        after = {}
        unconfirmed = f" (could not confirm the reload: {e.message})"
    else:
        unconfirmed = ""
    if after.get("change_error"):
        return f"**added, and the robot could not re-read it:** {after['change_error']}"

    name = skill["name"]
    try:
        ran = LINK.call("robot.do", {"skill": name})
    except RpcError as e:
        return (
            f"**`{name}` is installed** ({skill['duration']:g}s) **and it would not run:** "
            f"{e.message}"
        )
    said_no = not_accepted(ran)
    if said_no:
        return (
            f"**`{name}` is installed** ({skill['duration']:g}s) **and the robot would not run "
            f"it:** {said_no}"
        )
    return (
        f"**`{name}` installed and running** — {skill['duration']:g}s from "
        f"`{policy.key}`{already_note(ran)}{unconfirmed}"
    )


def run_installed(name: str) -> str:
    if not name:
        return "pick a skill first."
    try:
        ran = LINK.call("robot.do", {"skill": name})
    except RpcError as e:
        return f"**`{name}` would not run:** {e.message}"
    said_no = not_accepted(ran)
    if said_no:
        return f"**`{name}` would not run:** {said_no}"
    return f"**`{name}` running.**{already_note(ran)}"


def plain(method: str) -> str:
    """The three buttons that take no parameters: init, stop, relax."""
    try:
        result = LINK.call(method)
    except RpcError as e:
        return f"**`{method}` refused:** {e.message}"
    said_no = not_accepted(result)
    if said_no:
        return f"**`{method}` refused:** {said_no}"
    return f"**`{method}` done.**{already_note(result)}"


# ── the page ─────────────────────────────────────────────────────────────────


def row_text(policy: Policy) -> str:
    bits = [f"**{policy.name}** · {policy.headline()} · `{policy.origin}`"]
    if policy.description:
        bits.append(policy.description)
    blocked = catalogue.refusal(policy)
    warn = catalogue.caution(policy)
    if blocked:
        bits.append(f"⛔ {blocked}")
    elif warn:
        bits.append(f"⚠️ {warn}")
    if catalogue.needs_a_length(policy) and not blocked:
        bits.append("_declares no length — the seconds above are what it will be held for._")
    bits.append(f"<sub>`{policy.key}`</sub>")
    return "  \n".join(bits)


def load_catalogue(force: bool = False) -> list[Any]:
    """Render the rows, reading the Hub only when there is nothing to render.

    `demo.load` fires per page view and the read is twenty-odd HTTPS requests, so a Space
    with two visitors would spend its first four seconds fetching manifests it already has.
    *reload the catalogue* is the force, and a restart is the other one.
    """
    global CATALOGUE, CATALOGUE_ERROR, CATALOGUE_READ_AT
    if force or not CATALOGUE:
        CATALOGUE, CATALOGUE_ERROR = catalogue.read_hub()
        CATALOGUE_READ_AT = time.time()

    heading = (
        CATALOGUE_ERROR
        if CATALOGUE_ERROR
        else (
            f"{len(CATALOGUE)} policies on the Hub, official first — read "
            f"{time.strftime('%H:%M UTC', time.gmtime(CATALOGUE_READ_AT))}."
            # Gradio wants its components at build time, so the ceiling is real. Saying so is
            # the difference between a short list and a list that quietly lost its tail.
            + (
                f" Only the first {MAX_ROWS} are shown — raise `MAX_ROWS`."
                if len(CATALOGUE) > MAX_ROWS
                else ""
            )
        )
    )
    updates: list[Any] = [heading]
    for index in range(MAX_ROWS):
        if index < len(CATALOGUE):
            policy = CATALOGUE[index]
            updates.append(gr.update(visible=True))
            updates.append(gr.update(value=row_text(policy)))
            updates.append(
                gr.update(interactive=catalogue.refusal(policy) is None)
            )
        else:
            updates.append(gr.update(visible=False))
            updates.append(gr.update(value=""))
            updates.append(gr.update(interactive=False))
    return updates


def token_of(oauth: gr.OAuthToken | None) -> str:
    """A visitor's token by preference, and never the robot's.

    The rendezvous maps a token to one peer, so a consumer authenticating *as* the robot takes the
    robot off its owner's listing — the same fact §3.7 records about two robots sharing a
    credential. A visitor's OAuth token reaches their own robots and nobody else's, which is what
    makes a public Space defensible. `HF_TOKEN` is the fallback for a private one, and for running
    this file on a laptop.
    """
    if oauth is not None:
        return oauth.token
    return os.environ.get("HF_TOKEN", "").strip()


def find_robots(oauth: gr.OAuthToken | None) -> tuple[Any, str]:
    token = token_of(oauth)
    if not token:
        return gr.update(choices=[], value=None), (
            "sign in with Hugging Face, or set an `HF_TOKEN` secret on this Space."
        )
    ducks, note = LINK.robots(token)
    NAMES.update({peer: label for label, peer in ducks})
    return gr.update(choices=ducks, value=ducks[0][1] if ducks else None), note


def open_session(peer_id: str | None, oauth: gr.OAuthToken | None) -> str:
    if not peer_id:
        return "no duck chosen — press *find my ducks* first."
    token = token_of(oauth)
    if not token:
        return "sign in with Hugging Face, or set an `HF_TOKEN` secret on this Space."
    return LINK.connect(token, peer_id, NAMES.get(peer_id, peer_id))


with gr.Blocks(title="microduck policy shop") as demo:
    gr.Markdown(
        """
        # Put a policy on your duck

        Everything published to the Hub as `microduck-…`, read the way the robot reads it, with a
        button that downloads one onto your duck and runs it. Four calls per click:
        `policy.fetch`, `robot.setSkill`, `robot.policies`, `robot.do`.

        One consumer at a time — while this holds a session, the robot's own console cannot open
        one.
        """
    )

    with gr.Row():
        gr.LoginButton()
        find = gr.Button("find my ducks")
        chosen = gr.Dropdown(choices=[], label="your ducks", scale=2)
        connect = gr.Button("connect", variant="primary")
        disconnect = gr.Button("disconnect")

    link_state = gr.Markdown("**not connected.**")
    # What the last click did. Separate from the line above because that one repaints on a
    # timer, and a refusal worth reading would be gone a second after it arrived.
    status = gr.Markdown("")

    with gr.Row():
        read = gr.Button("read this robot")
        init = gr.Button("init (power the joints, stand)")
        stop = gr.Button("stop")
        relax = gr.Button("relax (cut torque — it will collapse)")

    with gr.Accordion("what this duck has now", open=True):
        robot_panel = gr.Markdown("Connect, then *read this robot*.")
        with gr.Row():
            installed = gr.Dropdown(choices=[], label="a skill it already has", scale=2)
            run = gr.Button("run it")

    gr.Markdown("## From the Hub")
    with gr.Row():
        hold = gr.Number(
            value=DEFAULT_HOLD,
            label="seconds to hold a policy that declares no length",
            precision=1,
            scale=2,
        )
        reload_catalogue = gr.Button("reload the catalogue")
    catalogue_note = gr.Markdown("Loading…")

    rows: list[Any] = []
    for index in range(MAX_ROWS):
        with gr.Row(visible=False) as row:
            text = gr.Markdown("")
            button = gr.Button("put it on the duck and run it", scale=0)
        button.click(
            lambda hold_s, index=index: install_and_run(index, hold_s),
            inputs=hold,
            outputs=status,
        ).then(lambda: robot_state()[0], outputs=robot_panel)
        rows.extend([row, text, button])

    with gr.Accordion("the control channel, line by line", open=False):
        wire = gr.Markdown("")

    find.click(find_robots, outputs=[chosen, status])
    connect.click(open_session, inputs=chosen, outputs=status).then(
        lambda: robot_state(), outputs=[robot_panel, installed]
    )
    disconnect.click(lambda: LINK.disconnect(), outputs=status)
    read.click(lambda: robot_state(), outputs=[robot_panel, installed])
    init.click(lambda: plain("robot.init"), outputs=status)
    stop.click(lambda: plain("robot.stop"), outputs=status)
    relax.click(lambda: plain("robot.relax"), outputs=status)
    run.click(run_installed, inputs=installed, outputs=status)
    reload_catalogue.click(
        lambda: load_catalogue(force=True), outputs=[catalogue_note, *rows]
    )

    demo.load(load_catalogue, outputs=[catalogue_note, *rows])
    # Once a second: it repaints a status line and a transcript, and the session it describes
    # changes state on its own — an ICE failure two minutes in should not need a click to appear.
    gr.Timer(1.0).tick(
        lambda: (LINK.describe(), LINK.transcript()), outputs=[link_state, wire]
    )


if __name__ == "__main__":
    demo.launch(server_name="0.0.0.0", server_port=int(os.environ.get("PORT", 7860)))
