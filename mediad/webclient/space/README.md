---
title: microduck console
emoji: 🦆
colorFrom: yellow
colorTo: gray
sdk: static
app_file: index.html
pinned: false
hf_oauth: true
# `openid profile` is always included and is all this needs: the token's whole job is proving an
# identity to the rendezvous service, which resolves it through `whoami-v2` and reads the username
# out of the answer. Asking for repository scopes here would be the mistake
# `remote-access-design.md` §2.4 is about fixing on the robot.
hf_oauth_expiration_minutes: 480
---

# microduck console

Drives a duck that is not on your network. Sign in with Hugging Face, and the robots your account
owns appear — the same console page the robot serves on its own LAN, reaching it through the
rendezvous service instead of a WebSocket.

**Do not edit this Space directly.** The page is
[`mediad/webclient/index.html`](https://github.com/pollen-robotics/microduck/blob/main/mediad/webclient/index.html)
in `pollen-robotics/microduck`, and `scripts/publish-console.sh` is what puts it here. It has to
live there because it tracks two things that do: the signalling protocol and the robot's own method
names. `docs/design/remote-access-design.md` §5.

One page, two transports. Served by a robot, it opens `ws://<robot>:8443`. Served from here — over
https, where a browser will not open a `ws://` at all — it reads the rendezvous service's event
stream and posts back to it, carrying the same envelopes with per-hop ids.
