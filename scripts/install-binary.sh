#!/bin/sh
set -eu

SOURCE_ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
INSTALL_ROOT=${HERDR_AGENT_CONTEXT_INSTALL_ROOT:-$SOURCE_ROOT}
MANIFEST="$SOURCE_ROOT/herdr-plugin.toml"
REPOSITORY=${HERDR_AGENT_CONTEXT_REPOSITORY:-ryonakae/herdr-agent-context}
VERSION=${HERDR_AGENT_CONTEXT_VERSION:-$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$MANIFEST" | head -n 1)}
OS=${HERDR_AGENT_CONTEXT_OS:-$(uname -s)}
ARCH=${HERDR_AGENT_CONTEXT_ARCH:-$(uname -m)}

case "$OS:$ARCH" in
    Darwin:arm64|Darwin:aarch64) TARGET=aarch64-apple-darwin ;;
    Darwin:x86_64|Darwin:amd64) TARGET=x86_64-apple-darwin ;;
    Linux:aarch64|Linux:arm64) TARGET=aarch64-unknown-linux-gnu ;;
    Linux:x86_64|Linux:amd64) TARGET=x86_64-unknown-linux-gnu ;;
    *) echo "herdr-agent-context: unsupported target: $OS $ARCH" >&2; exit 1 ;;
esac

ASSET="herdr-agent-context-v${VERSION}-${TARGET}.tar.gz"
BASE_URL=${HERDR_AGENT_CONTEXT_BASE_URL:-"https://github.com/${REPOSITORY}/releases/download/v${VERSION}"}

if [ "${1:-}" = "--print-asset" ]; then
    printf '%s\n' "$ASSET"
    exit 0
fi
if [ "$#" -ne 0 ]; then
    echo "usage: install-binary.sh [--print-asset]" >&2
    exit 2
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-install.XXXXXX")
cleanup() {
    rm -rf "$TMP"
}
trap cleanup EXIT HUP INT TERM

download() {
    url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        curl -fL --silent --show-error --retry 2 --connect-timeout 10 -o "$destination" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$destination" "$url"
    else
        echo "herdr-agent-context: curl or wget is required" >&2
        return 1
    fi
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        echo "herdr-agent-context: sha256sum or shasum is required" >&2
        return 1
    fi
}

download "$BASE_URL/$ASSET" "$TMP/$ASSET"
download "$BASE_URL/SHA256SUMS" "$TMP/SHA256SUMS"
EXPECTED=$(awk -v name="$ASSET" '$2 == name || $2 == ("*" name) { print $1; count += 1 } END { if (count != 1) exit 1 }' "$TMP/SHA256SUMS") || {
    echo "herdr-agent-context: checksum entry is missing or duplicated for $ASSET" >&2
    exit 1
}
ACTUAL=$(sha256 "$TMP/$ASSET")
if [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "herdr-agent-context: checksum verification failed for $ASSET" >&2
    exit 1
fi

CONTENTS=$(tar -tzf "$TMP/$ASSET" | LC_ALL=C sort)
if [ "$CONTENTS" != "LICENSE
herdr-agent-context" ]; then
    echo "herdr-agent-context: release archive has unexpected contents" >&2
    exit 1
fi
tar -xzf "$TMP/$ASSET" -C "$TMP"
test -f "$TMP/herdr-agent-context" || {
    echo "herdr-agent-context: release archive does not contain the binary" >&2
    exit 1
}
mkdir -p "$INSTALL_ROOT/bin"
STAGED="$INSTALL_ROOT/bin/.herdr-agent-context.new.$$"
cp "$TMP/herdr-agent-context" "$STAGED"
chmod 755 "$STAGED"
mv -f "$STAGED" "$INSTALL_ROOT/bin/herdr-agent-context"
printf 'herdr-agent-context: installed %s\n' "$ASSET"
