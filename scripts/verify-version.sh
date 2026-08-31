#!/bin/sh
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: verify-version.sh TAG [ROOT]" >&2
    exit 2
fi

TAG=$1
ROOT=${2:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
SCRIPTS=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

fail() {
    echo "herdr-agent-context: $1" >&2
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
            if (package_count != 1 || version_count != 1) exit 1
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

case "$TAG" in
    v*) VERSION=${TAG#v} ;;
    *) fail "tag must be a stable vX.Y.Z value" ;;
esac
valid_version "$VERSION" || fail "tag must be a stable vX.Y.Z value"

for file in "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$ROOT/herdr-plugin.toml" "$ROOT/CHANGELOG.md"; do
    test -f "$file" || fail "required file is missing: $file"
done
CARGO_VERSION=$(cargo_version "$ROOT/Cargo.toml") || fail "could not read root Cargo.toml package version"
LOCK_VERSION=$(lock_version "$ROOT/Cargo.lock") || fail "could not read root Cargo.lock package version"
PLUGIN_VERSION=$(plugin_version "$ROOT/herdr-plugin.toml") || fail "could not read root herdr-plugin.toml version"
for version in "$CARGO_VERSION" "$LOCK_VERSION" "$PLUGIN_VERSION"; do
    valid_version "$version" || fail "release versions must be stable X.Y.Z values"
done
if [ "$CARGO_VERSION" != "$LOCK_VERSION" ] || [ "$CARGO_VERSION" != "$PLUGIN_VERSION" ] || [ "$VERSION" != "$CARGO_VERSION" ]; then
    fail "version mismatch (cargo=$CARGO_VERSION lock=$LOCK_VERSION plugin=$PLUGIN_VERSION tag=$TAG)"
fi
HERDR_AGENT_CONTEXT_ROOT="$ROOT" sh "$SCRIPTS/release-notes.sh" check "$VERSION" >/dev/null ||
    fail "latest CHANGELOG.md version does not match $VERSION"
printf 'herdr-agent-context: version %s is consistent\n' "$CARGO_VERSION"
