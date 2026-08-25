#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
VERIFY="$ROOT/scripts/verify-release-assets.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-release-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
DIST="$TMP/dist"
STAGING="$TMP/staging"
mkdir -p "$DIST" "$STAGING"
printf '#!/bin/sh\necho release\n' >"$STAGING/herdr-agent-context"
chmod 755 "$STAGING/herdr-agent-context"
cp "$ROOT/LICENSE" "$STAGING/LICENSE"
TARGETS='aarch64-apple-darwin
x86_64-apple-darwin
aarch64-unknown-linux-gnu
x86_64-unknown-linux-gnu'

checksums() {
    directory=$1
    : >"$directory/SHA256SUMS"
    for asset in "$directory"/herdr-agent-context-v*.tar.gz; do
        name=$(basename "$asset")
        if command -v sha256sum >/dev/null 2>&1; then
            sum=$(sha256sum "$asset" | awk '{print $1}')
        else
            sum=$(shasum -a 256 "$asset" | awk '{print $1}')
        fi
        printf '%s  %s\n' "$sum" "$name" >>"$directory/SHA256SUMS"
    done
}

for target in $TARGETS; do
    tar -czf "$DIST/herdr-agent-context-v0.3.0-${target}.tar.gz" \
        -C "$STAGING" herdr-agent-context LICENSE
done
checksums "$DIST"
"$VERIFY" 0.3.0 "$DIST" >/dev/null

cp -R "$DIST" "$TMP/missing"
rm "$TMP/missing/herdr-agent-context-v0.3.0-aarch64-apple-darwin.tar.gz"
if "$VERIFY" 0.3.0 "$TMP/missing" >/dev/null 2>&1; then
    echo "release test: missing archive unexpectedly passed" >&2
    exit 1
fi

cp -R "$DIST" "$TMP/extra"
cp "$DIST/herdr-agent-context-v0.3.0-aarch64-apple-darwin.tar.gz" \
    "$TMP/extra/herdr-agent-context-v0.3.0-extra-target.tar.gz"
checksums "$TMP/extra"
if "$VERIFY" 0.3.0 "$TMP/extra" >/dev/null 2>&1; then
    echo "release test: extra archive unexpectedly passed" >&2
    exit 1
fi

cp -R "$DIST" "$TMP/corrupt"
printf 'corruption' >>"$TMP/corrupt/herdr-agent-context-v0.3.0-x86_64-apple-darwin.tar.gz"
if "$VERIFY" 0.3.0 "$TMP/corrupt" >/dev/null 2>&1; then
    echo "release test: corrupt archive unexpectedly passed" >&2
    exit 1
fi

cp -R "$DIST" "$TMP/contents"
mkdir "$TMP/extra-file"
printf 'unexpected\n' >"$TMP/extra-file/README"
tar -czf "$TMP/contents/herdr-agent-context-v0.3.0-aarch64-apple-darwin.tar.gz" \
    -C "$STAGING" herdr-agent-context LICENSE -C "$TMP/extra-file" README
checksums "$TMP/contents"
if "$VERIFY" 0.3.0 "$TMP/contents" >/dev/null 2>&1; then
    echo "release test: unexpected archive contents passed" >&2
    exit 1
fi

cp -R "$DIST" "$TMP/symlink"
mkdir "$TMP/symlink-stage"
cp "$ROOT/LICENSE" "$TMP/symlink-stage/LICENSE"
chmod 755 "$TMP/symlink-stage/LICENSE"
ln -s LICENSE "$TMP/symlink-stage/herdr-agent-context"
tar -czf "$TMP/symlink/herdr-agent-context-v0.3.0-aarch64-apple-darwin.tar.gz" \
    -C "$TMP/symlink-stage" herdr-agent-context LICENSE
checksums "$TMP/symlink"
if "$VERIFY" 0.3.0 "$TMP/symlink" >/dev/null 2>&1; then
    echo "release test: symlink binary unexpectedly passed" >&2
    exit 1
fi

cp -R "$DIST" "$TMP/license-symlink"
mkdir "$TMP/license-symlink-stage"
cp "$STAGING/herdr-agent-context" "$TMP/license-symlink-stage/herdr-agent-context"
ln -s herdr-agent-context "$TMP/license-symlink-stage/LICENSE"
tar -czf "$TMP/license-symlink/herdr-agent-context-v0.3.0-aarch64-apple-darwin.tar.gz" \
    -C "$TMP/license-symlink-stage" herdr-agent-context LICENSE
checksums "$TMP/license-symlink"
if "$VERIFY" 0.3.0 "$TMP/license-symlink" >/dev/null 2>&1; then
    echo "release test: symlink license unexpectedly passed" >&2
    exit 1
fi

cp -R "$DIST" "$TMP/hardlink"
mkdir "$TMP/hardlink-stage"
cp "$ROOT/LICENSE" "$TMP/hardlink-stage/LICENSE"
chmod 755 "$TMP/hardlink-stage/LICENSE"
ln "$TMP/hardlink-stage/LICENSE" "$TMP/hardlink-stage/herdr-agent-context"
tar -czf "$TMP/hardlink/herdr-agent-context-v0.3.0-aarch64-apple-darwin.tar.gz" \
    -C "$TMP/hardlink-stage" LICENSE herdr-agent-context
checksums "$TMP/hardlink"
if "$VERIFY" 0.3.0 "$TMP/hardlink" >/dev/null 2>&1; then
    echo "release test: hardlink binary unexpectedly passed" >&2
    exit 1
fi

"$ROOT/scripts/verify-version.sh" v0.3.0 "$ROOT" >/dev/null
if "$ROOT/scripts/verify-version.sh" 0.3.0 "$ROOT" >/dev/null 2>&1; then
    echo "release test: bare version unexpectedly passed" >&2
    exit 1
fi
mkdir "$TMP/version"
cp "$ROOT/Cargo.toml" "$TMP/version/Cargo.toml"
cp "$ROOT/herdr-plugin.toml" "$TMP/version/herdr-plugin.toml"
if "$ROOT/scripts/verify-version.sh" v9.9.9 "$TMP/version" >/dev/null 2>&1; then
    echo "release test: mismatched version unexpectedly passed" >&2
    exit 1
fi

printf 'release asset tests passed\n'
