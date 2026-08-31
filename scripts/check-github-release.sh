#!/bin/sh
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "usage: check-github-release.sh X.Y.Z BODY_FILE [REPOSITORY]" >&2
    exit 2
fi

VERSION=$1
BODY_FILE=$2
REPOSITORY=${3:-}
TAG="v$VERSION"
SCRIPTS=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
GH_COMMAND=${GH_COMMAND:-gh}
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-github-release.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
    echo "check-github-release: $1" >&2
    exit 1
}

valid_version() {
    printf '%s\n' "$1" | awk '
        NR == 1 && /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/ { valid = 1 }
        NR != 1 { valid = 0 }
        END { exit !valid }
    '
}

valid_version "$VERSION" || fail "version must be a stable X.Y.Z value"
[ -f "$BODY_FILE" ] || fail "release body not found: $BODY_FILE"
if [ -n "$REPOSITORY" ]; then
    printf '%s\n' "$REPOSITORY" | awk '
        NR == 1 && /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/ { valid = 1 }
        NR != 1 { valid = 0 }
        END { exit !valid }
    ' || fail "repository must be OWNER/NAME"
    API_ROOT="repos/$REPOSITORY"
else
    API_ROOT='repos/{owner}/{repo}'
fi

command -v jq >/dev/null 2>&1 || fail "jq is required"
sh "$SCRIPTS/release-notes.sh" verify "$VERSION" "$BODY_FILE" >/dev/null ||
    fail "expected release body does not satisfy the generated-note contract"

api_request() {
    endpoint=$1
    name=$2
    response="$TMP/$name.response"
    error="$TMP/$name.error"
    API_BODY="$TMP/$name.json"

    if "$GH_COMMAND" api --method GET --include "$endpoint" >"$response" 2>"$error"; then
        API_EXIT=0
    else
        API_EXIT=$?
    fi
    API_STATUS=$(awk '
        NR == 1 {
            sub(/\r$/, "")
            if ($1 ~ /^HTTP\// && $2 ~ /^[0-9][0-9][0-9]$/) print $2
        }
    ' "$response")
    if ! awk '
        BEGIN { separator = 0 }
        {
            line = $0
            sub(/\r$/, "", line)
            if (!separator && line == "") separator = 1
        }
        END { exit !separator }
    ' "$response"; then
        API_STATUS=
        : >"$API_BODY"
        return
    fi
    awk '
        body { print }
        {
            line = $0
            sub(/\r$/, "", line)
            if (!body && line == "") body = 1
        }
    ' "$response" >"$API_BODY"
}

api_request "$API_ROOT/releases/tags/$TAG" tag
if [ "$API_EXIT" -ne 0 ]; then
    if [ "$API_STATUS" = 404 ]; then
        printf 'absent\n'
        exit 0
    fi
    [ -n "$API_STATUS" ] || fail "tag release lookup failed before receiving an HTTP response"
    fail "tag release lookup failed with HTTP $API_STATUS"
fi
[ "$API_STATUS" = 200 ] || fail "tag release lookup returned unexpected HTTP ${API_STATUS:-response}"

if ! jq -e --arg tag "$TAG" '
    type == "object" and
    .tag_name == $tag and
    .name == $tag and
    .draft == false and
    .prerelease == false and
    (.body | type == "string") and
    (.assets | type == "array") and
    all(.assets[]; type == "object" and (.name | type == "string"))
' "$API_BODY" >/dev/null 2>&1; then
    fail "tag release JSON or stable release state does not match $TAG"
fi

jq -j '.body' "$API_BODY" >"$TMP/release-body.md" || fail "could not read release body"
sh "$SCRIPTS/release-notes.sh" verify "$VERSION" "$TMP/release-body.md" >/dev/null ||
    fail "existing release body does not satisfy the generated-note contract"

cat >"$TMP/expected-assets" <<EOF
SHA256SUMS
herdr-agent-context-v$VERSION-aarch64-apple-darwin.tar.gz
herdr-agent-context-v$VERSION-aarch64-unknown-linux-gnu.tar.gz
herdr-agent-context-v$VERSION-x86_64-apple-darwin.tar.gz
herdr-agent-context-v$VERSION-x86_64-unknown-linux-gnu.tar.gz
EOF
LC_ALL=C sort "$TMP/expected-assets" -o "$TMP/expected-assets"
if ! jq -r '.assets[].name' "$API_BODY" | LC_ALL=C sort >"$TMP/actual-assets"; then
    fail "could not read release asset names"
fi
if ! cmp -s "$TMP/expected-assets" "$TMP/actual-assets"; then
    fail "existing release assets do not match the exact five-name contract"
fi

api_request "$API_ROOT/releases/latest" latest
if [ "$API_EXIT" -ne 0 ]; then
    [ -n "$API_STATUS" ] || fail "latest release lookup failed before receiving an HTTP response"
    fail "latest release lookup failed with HTTP $API_STATUS"
fi
[ "$API_STATUS" = 200 ] || fail "latest release lookup returned unexpected HTTP ${API_STATUS:-response}"
if ! jq -e --arg tag "$TAG" 'type == "object" and .tag_name == $tag' "$API_BODY" >/dev/null 2>&1; then
    fail "$TAG is not the repository latest release or latest JSON is malformed"
fi

printf 'existing\n'
