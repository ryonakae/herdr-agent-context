#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/install-binary.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-installer-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
DIST="$TMP/dist"
STAGING="$TMP/staging"
INSTALL="$TMP/install"
mkdir -p "$DIST" "$STAGING" "$INSTALL/bin"
printf '#!/bin/sh\necho installed\n' >"$STAGING/herdr-agent-context"
chmod 755 "$STAGING/herdr-agent-context"
cp "$ROOT/LICENSE" "$STAGING/LICENSE"
ASSET='herdr-agent-context-v0.1.0-aarch64-apple-darwin.tar.gz'
tar -czf "$DIST/$ASSET" -C "$STAGING" herdr-agent-context LICENSE
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$DIST" && sha256sum "$ASSET" >SHA256SUMS)
else
    sum=$(shasum -a 256 "$DIST/$ASSET" | awk '{print $1}')
    printf '%s  %s\n' "$sum" "$ASSET" >"$DIST/SHA256SUMS"
fi

asset=$(HERDR_AGENT_CONTEXT_OS=Darwin HERDR_AGENT_CONTEXT_ARCH=arm64 "$SCRIPT" --print-asset)
test "$asset" = "$ASSET"
test "$(HERDR_AGENT_CONTEXT_OS=Darwin HERDR_AGENT_CONTEXT_ARCH=x86_64 "$SCRIPT" --print-asset)" = "herdr-agent-context-v0.1.0-x86_64-apple-darwin.tar.gz"
test "$(HERDR_AGENT_CONTEXT_OS=Linux HERDR_AGENT_CONTEXT_ARCH=aarch64 "$SCRIPT" --print-asset)" = "herdr-agent-context-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
test "$(HERDR_AGENT_CONTEXT_OS=Linux HERDR_AGENT_CONTEXT_ARCH=amd64 "$SCRIPT" --print-asset)" = "herdr-agent-context-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
if HERDR_AGENT_CONTEXT_OS=Plan9 HERDR_AGENT_CONTEXT_ARCH=mips "$SCRIPT" --print-asset >/dev/null 2>&1; then
    echo "installer test: unsupported target unexpectedly succeeded" >&2
    exit 1
fi

HERDR_AGENT_CONTEXT_OS=Darwin \
HERDR_AGENT_CONTEXT_ARCH=arm64 \
HERDR_AGENT_CONTEXT_BASE_URL="file://$DIST" \
HERDR_AGENT_CONTEXT_INSTALL_ROOT="$INSTALL" \
"$SCRIPT" >/dev/null
cmp "$STAGING/herdr-agent-context" "$INSTALL/bin/herdr-agent-context"
test -x "$INSTALL/bin/herdr-agent-context"

printf 'existing-binary\n' >"$INSTALL/bin/herdr-agent-context"
cp -R "$DIST" "$TMP/bad-checksum"
printf '%064d  %s\n' 0 "$ASSET" >"$TMP/bad-checksum/SHA256SUMS"
if HERDR_AGENT_CONTEXT_OS=Darwin \
    HERDR_AGENT_CONTEXT_ARCH=arm64 \
    HERDR_AGENT_CONTEXT_BASE_URL="file://$TMP/bad-checksum" \
    HERDR_AGENT_CONTEXT_INSTALL_ROOT="$INSTALL" \
    "$SCRIPT" >/dev/null 2>&1; then
    echo "installer test: invalid checksum unexpectedly succeeded" >&2
    exit 1
fi
test "$(cat "$INSTALL/bin/herdr-agent-context")" = "existing-binary"

mkdir "$TMP/malformed"
tar -czf "$TMP/malformed/$ASSET" -C "$STAGING" LICENSE
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$TMP/malformed" && sha256sum "$ASSET" >SHA256SUMS)
else
    sum=$(shasum -a 256 "$TMP/malformed/$ASSET" | awk '{print $1}')
    printf '%s  %s\n' "$sum" "$ASSET" >"$TMP/malformed/SHA256SUMS"
fi
if HERDR_AGENT_CONTEXT_OS=Darwin \
    HERDR_AGENT_CONTEXT_ARCH=arm64 \
    HERDR_AGENT_CONTEXT_BASE_URL="file://$TMP/malformed" \
    HERDR_AGENT_CONTEXT_INSTALL_ROOT="$INSTALL" \
    "$SCRIPT" >/dev/null 2>&1; then
    echo "installer test: malformed archive unexpectedly succeeded" >&2
    exit 1
fi
test "$(cat "$INSTALL/bin/herdr-agent-context")" = "existing-binary"

checksum_fixture() {
    directory=$1
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$directory" && sha256sum "$ASSET" >SHA256SUMS)
    else
        sum=$(shasum -a 256 "$directory/$ASSET" | awk '{print $1}')
        printf '%s  %s\n' "$sum" "$ASSET" >"$directory/SHA256SUMS"
    fi
}

mkdir -p "$TMP/symlink-stage" "$TMP/symlink"
cp "$ROOT/LICENSE" "$TMP/symlink-stage/LICENSE"
chmod 755 "$TMP/symlink-stage/LICENSE"
ln -s LICENSE "$TMP/symlink-stage/herdr-agent-context"
tar -czf "$TMP/symlink/$ASSET" -C "$TMP/symlink-stage" herdr-agent-context LICENSE
checksum_fixture "$TMP/symlink"
if HERDR_AGENT_CONTEXT_OS=Darwin \
    HERDR_AGENT_CONTEXT_ARCH=arm64 \
    HERDR_AGENT_CONTEXT_BASE_URL="file://$TMP/symlink" \
    HERDR_AGENT_CONTEXT_INSTALL_ROOT="$INSTALL" \
    "$SCRIPT" >/dev/null 2>&1; then
    echo "installer test: symlink binary unexpectedly succeeded" >&2
    exit 1
fi

mkdir -p "$TMP/license-symlink-stage" "$TMP/license-symlink"
cp "$STAGING/herdr-agent-context" "$TMP/license-symlink-stage/herdr-agent-context"
ln -s herdr-agent-context "$TMP/license-symlink-stage/LICENSE"
tar -czf "$TMP/license-symlink/$ASSET" -C "$TMP/license-symlink-stage" herdr-agent-context LICENSE
checksum_fixture "$TMP/license-symlink"
if HERDR_AGENT_CONTEXT_OS=Darwin \
    HERDR_AGENT_CONTEXT_ARCH=arm64 \
    HERDR_AGENT_CONTEXT_BASE_URL="file://$TMP/license-symlink" \
    HERDR_AGENT_CONTEXT_INSTALL_ROOT="$INSTALL" \
    "$SCRIPT" >/dev/null 2>&1; then
    echo "installer test: symlink license unexpectedly succeeded" >&2
    exit 1
fi

mkdir -p "$TMP/hardlink-stage" "$TMP/hardlink"
cp "$ROOT/LICENSE" "$TMP/hardlink-stage/LICENSE"
chmod 755 "$TMP/hardlink-stage/LICENSE"
ln "$TMP/hardlink-stage/LICENSE" "$TMP/hardlink-stage/herdr-agent-context"
tar -czf "$TMP/hardlink/$ASSET" -C "$TMP/hardlink-stage" LICENSE herdr-agent-context
checksum_fixture "$TMP/hardlink"
if HERDR_AGENT_CONTEXT_OS=Darwin \
    HERDR_AGENT_CONTEXT_ARCH=arm64 \
    HERDR_AGENT_CONTEXT_BASE_URL="file://$TMP/hardlink" \
    HERDR_AGENT_CONTEXT_INSTALL_ROOT="$INSTALL" \
    "$SCRIPT" >/dev/null 2>&1; then
    echo "installer test: hardlink binary unexpectedly succeeded" >&2
    exit 1
fi
test "$(cat "$INSTALL/bin/herdr-agent-context")" = "existing-binary"

printf 'installer tests passed\n'
