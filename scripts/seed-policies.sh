#!/bin/sh
# Install the official policy set, downloading it from the Hugging Face Hub.
#
# `robotd` reads its policies from /opt/robot/policies/current, deliberately outside the release:
# a gait retrain should not need a daemon release, and a daemon fix should not re-ship six
# megabytes of unchanged weights (docs/design/policy-channel-design.md §9). This is what fills
# that directory, and it fetches rather than copies — the same arrangement `setup-board.sh` uses
# for ONNX Runtime and `setup-gstreamer.sh` for the plugins, which are the other two things a
# board needs and a release has no business carrying.
#
# Run by `hooks/postinstall` on every update and by `scripts/install.sh` on a fresh board, which
# between them are every way policies reach a robot.
#
# THE RULE THAT MATTERS: never touch a set this script did not install. `current` pointing at
# anything but a `seed-*` directory means something else put policies there — `robotctl policy`,
# or whatever ends up publishing bundles — and replacing that would silently undo it on the next
# unrelated daemon update. The handover needs no flag and no config.
#
# Nothing here is signed and that is deliberate: a policy is not a binary. `robotd` holds the only
# write handle to the bus behind joint clamps, a fall reflex and an intent deadman, and refuses
# any graph that is not obs[1,61] -> actions[1,14] while the robot is standing still. That gate is
# also what catches a truncated download, which is why there are no hashes pinned here to go stale
# on every retrain.
#
# Never fatal. A robot with no policy holds its pose and reports *degraded* — a board to fix, not
# a release to roll back.
#
# A board that cannot reach the Hub on a first install ends up with no policies, and that is the
# accepted shape rather than an oversight: `robotd` holds its pose and reports *degraded*, the
# update gate passes, and the next update fetches. It is the same bargain `setup-board.sh` makes
# for ONNX Runtime — the board prerequisites need a network once.
#
# Usage: seed-policies.sh [POLICY_ROOT]
# Defaults to what a robot uses; the argument exists so this can be tested off a board.
set -eu

POLICY_ROOT="${1:-/opt/robot/policies}"

# The pin. An xtask test asserts these literals match `[workspace.metadata.policies]` in
# Cargo.toml — this script runs from inside a release and cannot read the manifest, which is the
# same reason `setup-gstreamer.sh` carries its plugin version as a literal.
#
# A floor, not a ceiling: it decides what a board installs when it has nothing, and a board moves
# past it with `robotctl policy update`, which needs no daemon release. Bumping this does, since
# it ships inside one — so bump it when a new set should be what *fresh* boards get.
POLICY_REPO="${POLICY_REPO:-pollen-robotics/microduck-policies}"
POLICY_VERSION="${POLICY_VERSION:-v1}"
POLICY_BASE_URL="${POLICY_BASE_URL:-https://huggingface.co/${POLICY_REPO}/resolve/${POLICY_VERSION}}"

# Every file a policy slot can default to, across both drive modes. An xtask test asserts this
# list is exactly what `robotd-params` resolves to — a name here that robotd never asks for is
# dead weight, and one robotd asks for that is missing is a slot that will not load.
POLICY_FILES="alpha_walking.onnx alpha_stand.onnx alpha_sitstand.onnx alpha_ground_pick.onnx ball_kick_left.onnx ball_kick_right.onnx roller.onnx roller_crouch.onnx roulade.onnx"

# Per-file, because `hooks/postinstall` runs inside an update under a 120-second hook timeout and
# a hook that times out fails the update and rolls it back. Nine files at eight seconds is 72,
# which leaves the rest of the hook room; a link that cannot move 800 KB in eight seconds is one
# the fallback below is for, and the next update tries again.
CURL_OPTS="--fail --location --silent --show-error --connect-timeout 5 --max-time 8"

# Where a set came from, written beside it.
#
# It is what lets `robotctl policy check` ask the Hub whether there is anything newer without
# anybody configuring the repo a second time. One writer, one copy, no drift — and a set that a
# different tool installs later carries its own, so "what is this and where is it from" has the
# same answer however it arrived.
#
# Only written when missing, so an update does not rewrite the file just to change a timestamp.
write_source() {
    [ ! -f "$1/.source" ] || return 0
    {
        echo "repo=${POLICY_REPO}"
        echo "version=${2}"
        echo "fetched=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } > "$1/.source" || echo "seed-policies: cannot record where this set came from" >&2
}

target="releases/seed-${POLICY_VERSION}"
live="$(readlink "${POLICY_ROOT}/current" 2>/dev/null || true)"

case "$live" in
    "$target")
        # Already the pinned set, so no network is needed — but back-fill the provenance record
        # if it is missing. A board seeded before that record existed would otherwise never gain
        # one, because this is the branch it takes forever after, and `policy check` would report
        # a robot with a perfectly good policy set as having nothing installed.
        write_source "${POLICY_ROOT}/${target}" "${POLICY_VERSION}"
        exit 0 ;;
    "")
        ;;                         # nothing installed yet
    releases/seed-*)
        ;;                         # an older set of ours, to be replaced
    *)
        echo "seed-policies: ${POLICY_ROOT}/current is not ours; leaving it alone" >&2
        exit 0 ;;
esac

staging="${POLICY_ROOT}/releases/.staging"
rm -rf "$staging"
mkdir -p "$staging" || { echo "seed-policies: cannot create ${staging}" >&2; exit 0; }

# Everything into staging first, so a partial download is never what `current` points at.
ok=yes
for name in $POLICY_FILES; do
    # shellcheck disable=SC2086  # CURL_OPTS is a deliberate word list
    if ! curl $CURL_OPTS -o "${staging}/${name}" "${POLICY_BASE_URL}/${name}"; then
        echo "seed-policies: could not fetch ${name} from ${POLICY_BASE_URL}" >&2
        ok=no
        break
    fi
done

if [ "$ok" = no ]; then
    # Nothing partial ever goes live, and nothing already installed is disturbed. A half-published
    # revision or a link that was down leaves the board exactly as it was — on the previous set if
    # it has one, with none if it does not — and the pin is retried at the next update.
    rm -rf "$staging"
    echo "seed-policies: leaving the policies already installed alone" >&2
    exit 0
fi

chmod 644 "$staging"/*.onnx 2>/dev/null || true

write_source "$staging" "${POLICY_VERSION}"

rm -rf "${POLICY_ROOT:?}/${target}"
mv "$staging" "${POLICY_ROOT}/${target}" \
    || { echo "seed-policies: cannot install into ${target}" >&2; exit 0; }

# `current -> releases/<something>`, relative to the directory the link is in, which is the layout
# the updater already uses and swaps (docs/design/updater-design.md §7.1). Relative and not
# absolute so the link keeps working wherever the root is — an absolute target silently resolves
# against the wrong directory the moment POLICY_ROOT is not what it was written with.
#
# Swapped rather than rewritten: a half-written `current` is one a restarting robotd could read.
# `mv -T` is the atomic form and is GNU-only; the fallback is a remove-and-relink, a smaller
# window rather than none. `rm -f` and not `rm -rf`, so a `current` that is somehow a real
# directory fails here instead of being deleted — the case above should have caught it, and if it
# did not, refusing is the right way to be wrong.
ln -sfn "$target" "${POLICY_ROOT}/current.new" || {
    echo "seed-policies: cannot stage ${POLICY_ROOT}/current" >&2
    exit 0
}
if ! mv -T "${POLICY_ROOT}/current.new" "${POLICY_ROOT}/current" 2>/dev/null; then
    if ! { rm -f "${POLICY_ROOT}/current" \
        && mv "${POLICY_ROOT}/current.new" "${POLICY_ROOT}/current"; }; then
        echo "seed-policies: cannot point ${POLICY_ROOT}/current at ${target}" >&2
        exit 0
    fi
fi

# Prune older sets of ours, keeping this one and the one it replaced.
#
# Every pin bump is a new directory, and during the transition every dev push is one too — seven
# megabytes apiece, in a directory nothing else prunes. The previous one is kept on purpose:
# **rollback does not run hooks** (updater/src/engine.rs — `post_swap` is on the apply path only),
# so reverting the daemon does not revert its policies, and pointing `current` back at the kept
# set by hand is the recovery when a policy is what went wrong.
#
# `seed-*` only, and only inside `releases/` — anything else under this root belongs to whatever
# installed it, which is the rule this script opens with.
for old_seed in "${POLICY_ROOT}"/releases/seed-*; do
    [ -d "$old_seed" ] || continue
    name="$(basename "$old_seed")"
    [ "releases/${name}" != "$target" ] || continue
    [ "releases/${name}" != "$live" ] || continue
    rm -rf "$old_seed" || echo "seed-policies: cannot remove ${old_seed}" >&2
done
