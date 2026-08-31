#!/bin/sh
# Seed the official policy set from a release, for as long as the release is the only source.
#
# `robotd` reads its policies from /opt/robot/policies/current rather than from inside the
# release directory, so that a gait retrain does not need a daemon release and a daemon fix does
# not re-ship six megabytes of unchanged weights (docs/design/policy-channel-design.md §9).
# Nothing publishes a policy bundle yet, so until something does, this is what fills that
# directory. It is a bootstrap for a destination that already exists, not a second home.
#
# Run by `hooks/postinstall` on every update and by `scripts/install.sh` on a fresh board, which
# between them are every way policies reach a robot today.
#
# THE RULE THAT MATTERS: never touch a set this script did not put there. `current` pointing at
# anything but a `seed-*` directory means something else installed policies — a bundle from the
# Hub, or whatever tool ends up publishing them — and overwriting that with the release's copies
# would silently undo it on the next unrelated daemon update. The handover therefore needs no
# flag and no config: the first real install ends the seeding for good.
#
# A newer daemon *does* replace an older seed of ours, because while the release is the only
# source of policies a daemon update is still how a retrained gait reaches a board. That stops
# being true the moment the line above does.
#
# Never fatal. A robot with no policy holds its pose and reports degraded — a board to fix, not a
# release to roll back.
#
# Usage: seed-policies.sh [RELEASE_DIR] [POLICY_ROOT]
# Both default to the values a robot uses; the arguments exist so this can be tested off a board.
set -eu

RELEASE_DIR="${1:-$PWD}"
POLICY_ROOT="${2:-/opt/robot/policies}"

[ -d "${RELEASE_DIR}/policies" ] || exit 0

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "${RELEASE_DIR}/version.toml" 2>/dev/null || true)"
if [ -z "$version" ]; then
    echo "seed-policies: no version in ${RELEASE_DIR}/version.toml; not seeding" >&2
    exit 0
fi

# `current -> releases/<something>`, relative to the directory the link is in, which is the
# layout the updater already uses and swaps (docs/design/updater-design.md §7.1). Relative and
# not absolute so the link keeps working wherever the root is — an absolute target silently
# resolves against the wrong directory the moment POLICY_ROOT is not what it was written with.
target="releases/seed-${version}"
seed="${POLICY_ROOT}/${target}"
live="$(readlink "${POLICY_ROOT}/current" 2>/dev/null || true)"

case "$live" in
    "$target")
        exit 0 ;;                  # this release has already seeded
    "")
        ;;                         # nothing installed yet
    releases/seed-*)
        ;;                         # an older seed of ours, to be replaced
    *)
        echo "seed-policies: ${POLICY_ROOT}/current is not ours; leaving it alone" >&2
        exit 0 ;;
esac

mkdir -p "$seed" || { echo "seed-policies: cannot create ${seed}" >&2; exit 0; }
for policy in "${RELEASE_DIR}"/policies/*.onnx; do
    [ -f "$policy" ] || continue
    install -m 644 "$policy" "${seed}/$(basename "$policy")" \
        || { echo "seed-policies: cannot seed $(basename "$policy")" >&2; exit 0; }
done

# Swapped rather than rewritten: a half-written `current` is one a restarting robotd could read.
# `mv -T` is the atomic form and is GNU-only; the fallback is a remove-and-relink, which is a
# smaller window rather than none. `rm -f` and not `rm -rf`, so that a `current` which is somehow
# a real directory fails here instead of being deleted — the case above should have caught it,
# and if it did not, refusing is the right way to be wrong.
ln -sfn "$target" "${POLICY_ROOT}/current.new" || {
    echo "seed-policies: cannot stage ${POLICY_ROOT}/current" >&2
    exit 0
}
if ! mv -T "${POLICY_ROOT}/current.new" "${POLICY_ROOT}/current" 2>/dev/null; then
    rm -f "${POLICY_ROOT}/current" \
        && mv "${POLICY_ROOT}/current.new" "${POLICY_ROOT}/current" \
        || echo "seed-policies: cannot point ${POLICY_ROOT}/current at ${seed}" >&2
fi
