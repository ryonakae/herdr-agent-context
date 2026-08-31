#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/validate-release-tag.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-release-tag-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

new_repository() {
    name=$1
    remote="$TMP/$name-remote.git"
    work="$TMP/$name-work"
    git init --bare "$remote" >/dev/null
    git init -b main "$work" >/dev/null
    git -C "$work" config user.email release-test@example.invalid
    git -C "$work" config user.name 'Release Test'
    cp "$ROOT/Cargo.toml" "$work/Cargo.toml"
    cp "$ROOT/Cargo.lock" "$work/Cargo.lock"
    cp "$ROOT/herdr-plugin.toml" "$work/herdr-plugin.toml"
    cp "$ROOT/CHANGELOG.md" "$work/CHANGELOG.md"
    git -C "$work" add Cargo.toml Cargo.lock herdr-plugin.toml CHANGELOG.md
    git -C "$work" commit -m 'release fixture' >/dev/null
    git -C "$work" remote add origin "$remote"
    git -C "$work" push -u origin main >/dev/null
    printf '%s\n' "$work"
}

expect_fail() {
    description=$1
    shift
    if "$@" >"$TMP/$description.out" 2>"$TMP/$description.err"; then
        echo "release tag test: $description unexpectedly passed" >&2
        exit 1
    fi
}

work=$(new_repository valid)
commit=$(git -C "$work" rev-parse HEAD)
test "$("$SCRIPT" v0.4.0 "$commit" "$work")" = "v0.4.0"

expect_fail bare-version "$SCRIPT" 0.4.0 "$commit" "$work"
expect_fail prerelease-tag "$SCRIPT" v0.4.0-rc.1 "$commit" "$work"
expect_fail build-tag "$SCRIPT" v0.4.0+build.1 "$commit" "$work"
expect_fail unknown-commit "$SCRIPT" v0.4.0 0000000000000000000000000000000000000000 "$work"

work=$(new_repository remote-main-advanced)
peer="$TMP/remote-main-advanced-peer"
git clone --branch main "$TMP/remote-main-advanced-remote.git" "$peer" >/dev/null
git -C "$peer" config user.email release-test@example.invalid
git -C "$peer" config user.name 'Release Test'
printf 'remote advance\n' >"$peer/remote-advance"
git -C "$peer" add remote-advance
git -C "$peer" commit -m 'remote main advance' >/dev/null
git -C "$peer" push origin main >/dev/null
advanced_commit=$(git -C "$peer" rev-parse HEAD)
test "$("$SCRIPT" v0.4.0 "$advanced_commit" "$work")" = "v0.4.0"
test "$(git -C "$work" rev-parse origin/main)" = "$advanced_commit"

work=$(new_repository source-mismatch)
commit=$(git -C "$work" rev-parse HEAD)
sed 's/^version = "0.4.0"$/version = "0.4.1"/' "$work/herdr-plugin.toml" >"$TMP/source-mismatch-plugin.toml"
mv "$TMP/source-mismatch-plugin.toml" "$work/herdr-plugin.toml"
expect_fail source-mismatch "$SCRIPT" v0.4.0 "$commit" "$work"

work=$(new_repository stale-changelog)
commit=$(git -C "$work" rev-parse HEAD)
sed 's/^## v0\.4\.0$/## v0.5.0/' "$work/CHANGELOG.md" >"$TMP/stale-changelog.md"
mv "$TMP/stale-changelog.md" "$work/CHANGELOG.md"
expect_fail stale-changelog "$SCRIPT" v0.4.0 "$commit" "$work"

work=$(new_repository topic-commit)
git -C "$work" checkout -b topic >/dev/null
printf 'topic\n' >"$work/topic"
git -C "$work" add topic
git -C "$work" commit -m 'topic commit' >/dev/null
commit=$(git -C "$work" rev-parse HEAD)
expect_fail non-main-ancestor "$SCRIPT" v0.4.0 "$commit" "$work"

printf 'release tag tests passed\n'
