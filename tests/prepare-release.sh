#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/prepare-release.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-prepare-release-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

new_root() {
    name=$1
    directory="$TMP/$name"
    mkdir -p "$directory"
    cat >"$directory/Cargo.toml" <<'EOF'
[package]
name = "herdr-agent-context"
version = "0.4.0"
edition = "2024"

[dependencies]
dependency = "0.4.0"
EOF
    cat >"$directory/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "dependency"
version = "0.4.0"
source = "registry+https://example.invalid/index"

[[package]]
name = "herdr-agent-context"
version = "0.4.0"
dependencies = [
 "dependency",
]
EOF
    cat >"$directory/herdr-plugin.toml" <<'EOF'
id = "ryonakae.agent-context"
name = "Agent Context"
version = "0.4.0"
EOF
    cat >"$directory/CHANGELOG.md" <<'EOF'
# Changelog

## v0.5.0

### Added

- Added a future release.

## v0.4.0

### Fixed

- Fixed the current release.
EOF
    chmod 640 "$directory/Cargo.toml"
    chmod 600 "$directory/Cargo.lock"
    chmod 644 "$directory/herdr-plugin.toml"
}

file_mode() {
    stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

snapshot() {
    directory=$1
    snapshot_directory="$TMP/snapshot-$2"
    mkdir "$snapshot_directory"
    cp "$directory/Cargo.toml" "$snapshot_directory/Cargo.toml"
    cp "$directory/Cargo.lock" "$snapshot_directory/Cargo.lock"
    cp "$directory/herdr-plugin.toml" "$snapshot_directory/herdr-plugin.toml"
}

assert_unchanged() {
    directory=$1
    snapshot_directory=$2
    cmp "$snapshot_directory/Cargo.toml" "$directory/Cargo.toml"
    cmp "$snapshot_directory/Cargo.lock" "$directory/Cargo.lock"
    cmp "$snapshot_directory/herdr-plugin.toml" "$directory/herdr-plugin.toml"
}

expect_failure_is_atomic() {
    description=$1
    directory=$2
    shift 2
    snapshot "$directory" "$description"
    snapshot_directory="$TMP/snapshot-$description"
    if HERDR_AGENT_CONTEXT_ROOT="$directory" "$SCRIPT" "$@" >"$TMP/$description.out" 2>"$TMP/$description.err"; then
        echo "prepare release test: $description unexpectedly passed" >&2
        exit 1
    fi
    assert_unchanged "$directory" "$snapshot_directory"
}

new_root valid
HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" 0.5.0 >/dev/null

grep -Fqx 'version = "0.5.0"' "$TMP/valid/Cargo.toml"
grep -Fqx 'version = "0.5.0"' "$TMP/valid/herdr-plugin.toml"
grep -Fqx 'name = "herdr-agent-context"' "$TMP/valid/Cargo.lock"
grep -Fqx 'version = "0.5.0"' "$TMP/valid/Cargo.lock"
grep -Fqx 'version = "0.4.0"' "$TMP/valid/Cargo.lock"
test "$(file_mode "$TMP/valid/Cargo.toml")" = 640
test "$(file_mode "$TMP/valid/Cargo.lock")" = 600
test "$(file_mode "$TMP/valid/herdr-plugin.toml")" = 644

new_root invalid-version
expect_failure_is_atomic invalid-version "$TMP/invalid-version" 0.5.0-rc.1

new_root missing-changelog
expect_failure_is_atomic missing-changelog "$TMP/missing-changelog" 0.6.0

new_root non-latest-changelog
expect_failure_is_atomic non-latest-changelog "$TMP/non-latest-changelog" 0.4.0

new_root divergent-sources
sed 's/^version = "0.4.0"$/version = "0.4.1"/' "$TMP/divergent-sources/herdr-plugin.toml" >"$TMP/divergent-sources/plugin.tmp"
mv "$TMP/divergent-sources/plugin.tmp" "$TMP/divergent-sources/herdr-plugin.toml"
expect_failure_is_atomic divergent-sources "$TMP/divergent-sources" 0.5.0

new_root missing-cargo-anchor
printf '[package]\nname = "herdr-agent-context"\n' >"$TMP/missing-cargo-anchor/Cargo.toml"
expect_failure_is_atomic missing-cargo-anchor "$TMP/missing-cargo-anchor" 0.5.0

new_root missing-plugin-anchor
printf 'id = "ryonakae.agent-context"\nname = "Agent Context"\n' >"$TMP/missing-plugin-anchor/herdr-plugin.toml"
expect_failure_is_atomic missing-plugin-anchor "$TMP/missing-plugin-anchor" 0.5.0

new_root missing-lock-anchor
printf 'version = 4\n' >"$TMP/missing-lock-anchor/Cargo.lock"
expect_failure_is_atomic missing-lock-anchor "$TMP/missing-lock-anchor" 0.5.0

new_root duplicate-lock-anchor
cat >>"$TMP/duplicate-lock-anchor/Cargo.lock" <<'EOF'

[[package]]
name = "herdr-agent-context"
version = "0.4.0"
EOF
expect_failure_is_atomic duplicate-lock-anchor "$TMP/duplicate-lock-anchor" 0.5.0

new_root malformed-lockfile
cat >"$TMP/malformed-lockfile/Cargo.lock" <<'EOF'
version = 4

[[package]]
version = "0.4.0"
name = "herdr-agent-context"
EOF
expect_failure_is_atomic malformed-lockfile "$TMP/malformed-lockfile" 0.5.0

printf 'prepare release tests passed\n'
