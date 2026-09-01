#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: verify-release-assets.sh VERSION DIST_DIR" >&2
    exit 2
fi

VERSION=${1#v}
DIST=$2
TARGETS='aarch64-apple-darwin
x86_64-apple-darwin
aarch64-unknown-linux-gnu
x86_64-unknown-linux-gnu'

fail() {
    echo "herdr-agent-context: $1" >&2
    exit 1
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        fail "sha256sum or shasum is required"
    fi
}

test -f "$DIST/SHA256SUMS" || fail "SHA256SUMS is missing"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-verify.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
link_count() {
    stat -c %h "$1" 2>/dev/null || stat -f %l "$1"
}
expected_names=
expected_count=0
for target in $TARGETS; do
    asset="herdr-agent-context-v${VERSION}-${target}.tar.gz"
    expected_names="${expected_names}${asset}
"
    test -f "$DIST/$asset" || fail "release asset is missing: $asset"
    contents=$(tar -tzf "$DIST/$asset" | LC_ALL=C sort) || fail "release asset is unreadable: $asset"
    test "$contents" = "LICENSE
herdr-agent-context" || fail "unexpected archive contents: $asset"
    extracted="$TMP/$target"
    mkdir "$extracted"
    tar -xzf "$DIST/$asset" -C "$extracted" || fail "release asset cannot be extracted: $asset"
    if [ ! -f "$extracted/herdr-agent-context" ] || [ -L "$extracted/herdr-agent-context" ] || [ ! -x "$extracted/herdr-agent-context" ]; then
        fail "release binary is not a regular executable: $asset"
    fi
    if [ ! -f "$extracted/LICENSE" ] || [ -L "$extracted/LICENSE" ]; then
        fail "release license is not a regular file: $asset"
    fi
    test "$(link_count "$extracted/herdr-agent-context")" -eq 1 || fail "release binary is linked: $asset"
    test "$(link_count "$extracted/LICENSE")" -eq 1 || fail "release license is linked: $asset"
    expected_count=$((expected_count + 1))
done

actual_count=$(find "$DIST" -maxdepth 1 -type f -name 'herdr-agent-context-v*.tar.gz' | wc -l | tr -d ' ')
test "$actual_count" -eq "$expected_count" || fail "expected $expected_count archives, found $actual_count"
checksum_count=$(awk 'NF == 2 { count += 1 } END { print count + 0 }' "$DIST/SHA256SUMS")
test "$checksum_count" -eq "$expected_count" || fail "SHA256SUMS must list exactly $expected_count assets"

for target in $TARGETS; do
    asset="herdr-agent-context-v${VERSION}-${target}.tar.gz"
    matches=$(awk -v name="$asset" '$2 == name || $2 == ("*" name) { count += 1 } END { print count + 0 }' "$DIST/SHA256SUMS")
    test "$matches" -eq 1 || fail "SHA256SUMS must list $asset exactly once"
    expected=$(awk -v name="$asset" '$2 == name || $2 == ("*" name) { print $1 }' "$DIST/SHA256SUMS")
    actual=$(sha256 "$DIST/$asset")
    test "$actual" = "$expected" || fail "checksum verification failed for $asset"
done

while read -r _ name extra; do
    test -z "${extra:-}" || fail "malformed SHA256SUMS entry"
    name=${name#\*}
    printf '%s' "$expected_names" | grep -Fqx "$name" || fail "unexpected checksum entry: $name"
done <"$DIST/SHA256SUMS"
