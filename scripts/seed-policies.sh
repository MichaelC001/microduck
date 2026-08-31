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
# Usage: seed-policies.sh [RELEASE_DIR] [POLICY_ROOT]
# Both default to what a robot uses; the arguments exist so this can be tested off a board.
set -eu

RELEASE_DIR="${1:-$PWD}"
POLICY_ROOT="${2:-/opt/robot/policies}"

# The pin. An xtask test asserts these literals match `[workspace.metadata.policies]` in
# Cargo.toml — this script runs from inside a release and cannot read the manifest, which is the
# same reason `setup-gstreamer.sh` carries its plugin version as a literal.
#
# A retrain bumps POLICY_VERSION, and that is the whole of "ship a new gait": no daemon release,
# no restart, and a board picks it up at its next update.
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

target="releases/seed-${POLICY_VERSION}"
live="$(readlink "${POLICY_ROOT}/current" 2>/dev/null || true)"

case "$live" in
    "$target")
        exit 0 ;;                  # the pinned set is already installed; no network needed
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
    rm -rf "$staging"

    # A set is already installed, so keep it. The fetch that just failed was for a *newer* pin —
    # a half-published revision, or a link that was down — and replacing a working gait with an
    # older copy to react to that would be a downgrade nobody asked for. The pin is retried at
    # the next update, and until then the robot walks the way it did this morning.
    if [ -n "$live" ]; then
        echo "seed-policies: keeping the set already installed (${live})" >&2
        exit 0
    fi

    # TRANSITIONAL, and only for a board with nothing at all. The release still carries a copy of
    # the set, so a first install that cannot reach the Hub gets a gait instead of no gait. Named
    # for the release rather than the pin, so the next update tries the Hub again rather than
    # reading this as the pinned set.
    #
    # This whole branch goes when `policies/` leaves the repository, which is the point of the
    # exercise; it is here so the Hub path can be proved on a real board without a duck that
    # cannot walk if it is wrong.
    if [ ! -d "${RELEASE_DIR}/policies" ]; then
        echo "seed-policies: no policies installed and none to fall back on" >&2
        exit 0
    fi
    version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "${RELEASE_DIR}/version.toml" 2>/dev/null || true)"
    [ -n "$version" ] || { echo "seed-policies: no version to name a fallback after" >&2; exit 0; }
    target="releases/seed-release-${version}"
    [ "$live" != "$target" ] || exit 0
    echo "seed-policies: falling back on the copy this release carries" >&2
    staging="${POLICY_ROOT}/${target}"
    rm -rf "$staging"
    mkdir -p "$staging" || { echo "seed-policies: cannot create ${staging}" >&2; exit 0; }
    for policy in "${RELEASE_DIR}"/policies/*.onnx; do
        [ -f "$policy" ] || continue
        install -m 644 "$policy" "${staging}/$(basename "$policy")" \
            || { echo "seed-policies: cannot copy $(basename "$policy")" >&2; exit 0; }
    done
else
    chmod 644 "$staging"/*.onnx 2>/dev/null || true
    rm -rf "${POLICY_ROOT:?}/${target}"
    mv "$staging" "${POLICY_ROOT}/${target}" \
        || { echo "seed-policies: cannot install into ${target}" >&2; exit 0; }
fi

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
