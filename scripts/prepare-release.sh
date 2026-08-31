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
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
write_cargo "$CARGO" "$VERSION" >"$TMP/Cargo.toml" || fail "could not prepare Cargo.toml"
write_lock "$LOCK" "$VERSION" >"$TMP/Cargo.lock" || fail "could not prepare Cargo.lock"
write_plugin "$PLUGIN" "$VERSION" >"$TMP/herdr-plugin.toml" || fail "could not prepare herdr-plugin.toml"
chmod "$(file_mode "$CARGO")" "$TMP/Cargo.toml" || fail "could not preserve Cargo.toml mode"
chmod "$(file_mode "$LOCK")" "$TMP/Cargo.lock" || fail "could not preserve Cargo.lock mode"
chmod "$(file_mode "$PLUGIN")" "$TMP/herdr-plugin.toml" || fail "could not preserve herdr-plugin.toml mode"

[ "$(cargo_version "$TMP/Cargo.toml")" = "$VERSION" ] || fail "prepared Cargo.toml is invalid"
[ "$(lock_version "$TMP/Cargo.lock")" = "$VERSION" ] || fail "prepared Cargo.lock is invalid"
[ "$(plugin_version "$TMP/herdr-plugin.toml")" = "$VERSION" ] || fail "prepared herdr-plugin.toml is invalid"

mv "$TMP/Cargo.toml" "$CARGO"
mv "$TMP/Cargo.lock" "$LOCK"
mv "$TMP/herdr-plugin.toml" "$PLUGIN"
printf '%s\n' "$VERSION"
