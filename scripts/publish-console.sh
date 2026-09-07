#!/usr/bin/env bash
#
# Publish the console page to the Hugging Face Space that serves it remotely.
#
# The page in this repository is the source (`remote-access-design.md` §5): it tracks the
# signalling protocol and the robot's method names, and a copy living in the Space would drift
# from both. This is the deploy — by hand while there is one Space, by CI when that stops being
# true.
#
# What it substitutes, and what it deliberately does not:
#
#   {{API_VERSION}}     the version this checkout speaks, read from `duck-ipc-proto`, so the page
#                       can tell a person that it and the robot disagree.
#   {{SIGNALLING_PORT}} left alone. The page reads an unsubstituted port as "no robot served me",
#                       which is exactly true here and is what selects the rendezvous transport.
#                       Substituting it would make the page try to open a WebSocket to the Space.
#
# Usage: scripts/publish-console.sh [--space <org/name>] [--dry-run]
#
# Pushing needs a Hugging Face token with write access to the Space. `hf auth login` stores one,
# and git will ask for it otherwise; the token is never read by this script.

set -euo pipefail

SPACE="pollen-robotics/microduck-console"
DRY_RUN=

while [ $# -gt 0 ]; do
    case "$1" in
        --space) SPACE="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
PAGE="$REPO_ROOT/mediad/webclient/index.html"
CARD="$REPO_ROOT/mediad/webclient/space/README.md"

for file in "$PAGE" "$CARD"; do
    [ -f "$file" ] || { echo "missing: $file" >&2; exit 1; }
done

# One source of truth for the wire version: the constant every daemon compiles against.
API_VERSION=$(sed -n 's/^pub const API_VERSION: u32 = \([0-9]*\);.*/\1/p' \
    "$REPO_ROOT/duck-ipc-proto/src/lib.rs")
[ -n "$API_VERSION" ] || { echo "could not read API_VERSION" >&2; exit 1; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

sed "s/{{API_VERSION}}/$API_VERSION/g" "$PAGE" > "$STAGE/index.html"
cp "$CARD" "$STAGE/README.md"

grep -q '{{SIGNALLING_PORT}}' "$STAGE/index.html" || {
    echo "the port token is gone from the page; the Space copy would try to open a WebSocket" >&2
    exit 1
}

echo "page:  $(wc -c < "$STAGE/index.html") bytes, api v$API_VERSION"
echo "space: https://huggingface.co/spaces/$SPACE"

if [ -n "$DRY_RUN" ]; then
    echo "--dry-run: staged in $STAGE, nothing pushed"
    trap - EXIT
    exit 0
fi

CLONE="$STAGE/space"
git clone --depth 1 "https://huggingface.co/spaces/$SPACE" "$CLONE"
cp "$STAGE/index.html" "$STAGE/README.md" "$CLONE/"

cd "$CLONE"
if git diff --quiet; then
    echo "the Space already serves this page"
    exit 0
fi

REVISION=$(cd "$REPO_ROOT" && git rev-parse --short HEAD)
git add index.html README.md
git commit -q -m "Console from microduck $REVISION (api v$API_VERSION)"
git push
echo "pushed. The Space rebuilds in a few seconds."
