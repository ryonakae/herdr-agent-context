#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: prepare-release.sh X.Y.Z" >&2
    exit 2
fi

ROOT=${HERDR_AGENT_CONTEXT_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
SCRIPTS=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
VERSION=$1
CARGO="$ROOT/Cargo.toml"
LOCK="$ROOT/Cargo.lock"
PLUGIN="$ROOT/herdr-plugin.toml"
MV_COMMAND=${HERDR_AGENT_CONTEXT_MV_COMMAND:-mv}
TMP=
COMMITTING=0
CARGO_MODE=
LOCK_MODE=
PLUGIN_MODE=

fail() {
    echo "prepare-release: $1" >&2
    exit 1
}

valid_version() {
    printf '%s\n' "$1" | awk '
        NR == 1 && /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/ { valid = 1 }
        NR != 1 { valid = 0 }
        END { exit !valid }
    '
}

cargo_version() {
    awk '
        function invalid() { invalid_input = 1 }
        /^\[package\]$/ { package = 1; package_count += 1; next }
        /^\[/ { package = 0 }
        package && /^version = "[^"]*"$/ {
            value = $0
            sub(/^version = "/, "", value)
            sub(/"$/, "", value)
            version = value
            version_count += 1
        }
        END {
            if (invalid_input || package_count != 1 || version_count != 1) exit 1
            print version
        }
    ' "$1"
}

plugin_version() {
    awk '
        BEGIN { root = 1 }
        /^\[/ { root = 0 }
        root && /^version = "[^"]*"$/ {
            value = $0
            sub(/^version = "/, "", value)
            sub(/"$/, "", value)
            version = value
            version_count += 1
        }
        END {
            if (version_count != 1) exit 1
            print version
        }
    ' "$1"
}

lock_version() {
    awk '
        function finish_package() {
            if (root) {
                root_count += 1
                if (root_version_count != 1) invalid_input = 1
                version = root_version
            }
        }
        /^\[\[package\]\]$/ {
            finish_package()
            in_package = 1
            root = 0
            root_version_count = 0
            root_version = ""
            next
        }
        in_package && /^name = "herdr-agent-context"$/ {
            if (root) invalid_input = 1
            root = 1
            next
        }
        in_package && root && /^version = "[^"]*"$/ {
            value = $0
            sub(/^version = "/, "", value)
            sub(/"$/, "", value)
            root_version = value
            root_version_count += 1
        }
        END {
            finish_package()
            if (invalid_input || root_count != 1) exit 1
            print version
        }
    ' "$1"
}

file_mode() {
    stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

restore_originals() {
    restore_failed=0
    cp "$TMP/original-Cargo.toml" "$CARGO" || restore_failed=1
    chmod "$CARGO_MODE" "$CARGO" || restore_failed=1
    cp "$TMP/original-Cargo.lock" "$LOCK" || restore_failed=1
    chmod "$LOCK_MODE" "$LOCK" || restore_failed=1
    cp "$TMP/original-herdr-plugin.toml" "$PLUGIN" || restore_failed=1
    chmod "$PLUGIN_MODE" "$PLUGIN" || restore_failed=1
    return "$restore_failed"
}

cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ "$COMMITTING" -eq 1 ] && ! restore_originals; then
        echo "prepare-release: could not restore original release files" >&2
        status=1
    fi
    if [ -n "$TMP" ]; then
        rm -rf "$TMP"
    fi
    exit "$status"
}

write_cargo() {
    awk -v version="$2" '
        /^\[package\]$/ { package = 1; print; next }
        /^\[/ { package = 0 }
        package && /^version = "[^"]*"$/ {
            print "version = \"" version "\""
            replaced += 1
            next
        }
        { print }
        END { if (replaced != 1) exit 1 }
    ' "$1"
}

write_plugin() {
    awk -v version="$2" '
        BEGIN { root = 1 }
        /^\[/ { root = 0 }
        root && /^version = "[^"]*"$/ {
            print "version = \"" version "\""
            replaced += 1
            next
        }
        { print }
        END { if (replaced != 1) exit 1 }
    ' "$1"
}

write_lock() {
    awk -v version="$2" '
        function finish_package() {
            if (root) {
                root_count += 1
                if (root_version_count != 1) invalid_input = 1
            }
        }
        /^\[\[package\]\]$/ {
            finish_package()
            in_package = 1
            root = 0
            root_version_count = 0
            print
            next
        }
        in_package && /^name = "herdr-agent-context"$/ {
            if (root) invalid_input = 1
            root = 1
            print
            next
        }
        in_package && root && /^version = "[^"]*"$/ {
            root_version_count += 1
            print "version = \"" version "\""
            next
        }
        { print }
        END {
            finish_package()
            if (invalid_input || root_count != 1) exit 1
        }
    ' "$1"
}

valid_version "$VERSION" || fail "version must be a stable X.Y.Z value"
for file in "$CARGO" "$LOCK" "$PLUGIN"; do
    test -f "$file" || fail "required file is missing: $file"
done

cargo_current=$(cargo_version "$CARGO") || fail "Cargo.toml must contain one root package version"
plugin_current=$(plugin_version "$PLUGIN") || fail "herdr-plugin.toml must contain one root version"
lock_current=$(lock_version "$LOCK") || fail "Cargo.lock must contain one root package version"
for current in "$cargo_current" "$plugin_current" "$lock_current"; do
    valid_version "$current" || fail "current release versions must be stable X.Y.Z values"
done
if [ "$cargo_current" != "$plugin_current" ] || [ "$cargo_current" != "$lock_current" ]; then
    fail "current release versions are not synchronized"
fi
HERDR_AGENT_CONTEXT_ROOT="$ROOT" sh "$SCRIPTS/release-notes.sh" check "$VERSION" >/dev/null ||
    fail "CHANGELOG.md must contain $VERSION as its latest valid release"

TMP=$(mktemp -d "$ROOT/.prepare-release.XXXXXX") || fail "could not create temporary directory"
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

CARGO_MODE=$(file_mode "$CARGO") || fail "could not read Cargo.toml mode"
LOCK_MODE=$(file_mode "$LOCK") || fail "could not read Cargo.lock mode"
PLUGIN_MODE=$(file_mode "$PLUGIN") || fail "could not read herdr-plugin.toml mode"
cp "$CARGO" "$TMP/original-Cargo.toml" || fail "could not back up Cargo.toml"
cp "$LOCK" "$TMP/original-Cargo.lock" || fail "could not back up Cargo.lock"
cp "$PLUGIN" "$TMP/original-herdr-plugin.toml" || fail "could not back up herdr-plugin.toml"
chmod "$CARGO_MODE" "$TMP/original-Cargo.toml" || fail "could not preserve Cargo.toml backup mode"
chmod "$LOCK_MODE" "$TMP/original-Cargo.lock" || fail "could not preserve Cargo.lock backup mode"
chmod "$PLUGIN_MODE" "$TMP/original-herdr-plugin.toml" || fail "could not preserve herdr-plugin.toml backup mode"

write_cargo "$CARGO" "$VERSION" >"$TMP/Cargo.toml" || fail "could not prepare Cargo.toml"
write_lock "$LOCK" "$VERSION" >"$TMP/Cargo.lock" || fail "could not prepare Cargo.lock"
write_plugin "$PLUGIN" "$VERSION" >"$TMP/herdr-plugin.toml" || fail "could not prepare herdr-plugin.toml"
chmod "$CARGO_MODE" "$TMP/Cargo.toml" || fail "could not preserve Cargo.toml mode"
chmod "$LOCK_MODE" "$TMP/Cargo.lock" || fail "could not preserve Cargo.lock mode"
chmod "$PLUGIN_MODE" "$TMP/herdr-plugin.toml" || fail "could not preserve herdr-plugin.toml mode"

[ "$(cargo_version "$TMP/Cargo.toml")" = "$VERSION" ] || fail "prepared Cargo.toml is invalid"
[ "$(lock_version "$TMP/Cargo.lock")" = "$VERSION" ] || fail "prepared Cargo.lock is invalid"
[ "$(plugin_version "$TMP/herdr-plugin.toml")" = "$VERSION" ] || fail "prepared herdr-plugin.toml is invalid"

COMMITTING=1
"$MV_COMMAND" "$TMP/Cargo.toml" "$CARGO" || fail "could not replace Cargo.toml"
"$MV_COMMAND" "$TMP/Cargo.lock" "$LOCK" || fail "could not replace Cargo.lock"
"$MV_COMMAND" "$TMP/herdr-plugin.toml" "$PLUGIN" || fail "could not replace herdr-plugin.toml"
COMMITTING=0
printf '%s\n' "$VERSION"
