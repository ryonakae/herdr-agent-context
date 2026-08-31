#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "usage: validate-release-tag.sh vX.Y.Z COMMIT [ROOT]" >&2
    exit 2
fi

TAG=$1
COMMIT=$2
ROOT=${3:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
SCRIPTS=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
GIT_COMMAND=${HERDR_AGENT_CONTEXT_GIT_COMMAND:-git}

fail() {
    echo "validate-release-tag: $1" >&2
    exit 1
}

valid_version() {
    printf '%s\n' "$1" | awk '
        NR == 1 && /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/ { valid = 1 }
        NR != 1 { valid = 0 }
        END { exit !valid }
    '
}

case "$TAG" in
    v*) VERSION=${TAG#v} ;;
    *) fail "tag must be a stable vX.Y.Z value" ;;
esac
valid_version "$VERSION" || fail "tag must be a stable vX.Y.Z value"
sh "$SCRIPTS/verify-version.sh" "$TAG" "$ROOT" >/dev/null ||
    fail "release-owned versions or latest CHANGELOG.md do not match $TAG"

if ! "$GIT_COMMAND" -C "$ROOT" fetch --no-tags origin main; then
    fail "could not fetch origin/main without tags"
fi
if ! "$GIT_COMMAND" -C "$ROOT" merge-base --is-ancestor "$COMMIT" origin/main; then
    fail "$COMMIT is not an ancestor of origin/main"
fi
printf '%s\n' "$TAG"
