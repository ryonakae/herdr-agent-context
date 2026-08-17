#!/bin/sh
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: verify-version.sh TAG [ROOT]" >&2
    exit 2
fi

TAG=$1
ROOT=${2:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
CARGO_VERSION=$(awk '
    /^\[package\]$/ { package = 1; next }
    /^\[/ { package = 0 }
    package && /^version = "/ { gsub(/^version = "|"$/, ""); print; exit }
' "$ROOT/Cargo.toml")
PLUGIN_VERSION=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$ROOT/herdr-plugin.toml" | head -n 1)
if [ -z "$CARGO_VERSION" ] || [ -z "$PLUGIN_VERSION" ]; then
    echo "herdr-agent-context: could not read package versions" >&2
    exit 1
fi
if [ "$CARGO_VERSION" != "$PLUGIN_VERSION" ] || [ "$TAG" != "v$CARGO_VERSION" ]; then
    echo "herdr-agent-context: version mismatch (cargo=$CARGO_VERSION plugin=$PLUGIN_VERSION tag=$TAG)" >&2
    exit 1
fi
printf 'herdr-agent-context: version %s is consistent\n' "$CARGO_VERSION"
