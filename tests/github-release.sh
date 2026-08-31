#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/check-github-release.sh"
WORKFLOW="$ROOT/.github/workflows/release.yml"
CI_WORKFLOW="$ROOT/.github/workflows/ci.yml"
VERSION=$(awk '/^version = "[^"]*"$/ { value = $0; sub(/^version = "/, "", value); sub(/"$/, "", value); print value; exit }' "$ROOT/herdr-plugin.toml")
TAG="v$VERSION"
REPOSITORY=ryonakae/herdr-agent-context
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-github-release-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

mkdir "$TMP/fixture"
sh "$ROOT/scripts/release-notes.sh" render "$VERSION" >"$TMP/notes.md"

cat >"$TMP/fake-gh" <<'EOF'
#!/bin/sh
set -eu

log=${FAKE_GH_LOG:?}
fixture=${FAKE_GH_FIXTURE:?}
printf '%s\n' "$*" >>"$log"

[ "${1:-}" = api ] || {
    echo "fake gh: unexpected command: $*" >&2
    exit 2
}
endpoint=
for argument in "$@"; do
    endpoint=$argument
done
case "$endpoint" in
    */releases/tags/*) response=tag ;;
    */releases/latest) response=latest ;;
    *)
        echo "fake gh: unexpected endpoint: $endpoint" >&2
        exit 2
        ;;
esac

if [ -f "$fixture/$response.transport" ]; then
    echo "fake gh: transport failure" >&2
    exit 1
fi
status=$(cat "$fixture/$response.status")
printf 'HTTP/2.0 %s Fake\r\n' "$status"
printf 'content-type: application/json\r\n\r\n'
cat "$fixture/$response.json"
printf '\n'
case "$status" in
    2??) exit 0 ;;
    *) exit 1 ;;
esac
EOF
chmod +x "$TMP/fake-gh"

exact_release_json() {
    jq -n \
        --arg tag "$TAG" \
        --rawfile body "$TMP/notes.md" \
        --arg version "$VERSION" '
        {
          tag_name: $tag,
          name: $tag,
          draft: false,
          prerelease: false,
          body: $body,
          assets: [
            {name: ("herdr-agent-context-v" + $version + "-aarch64-apple-darwin.tar.gz")},
            {name: ("herdr-agent-context-v" + $version + "-x86_64-apple-darwin.tar.gz")},
            {name: ("herdr-agent-context-v" + $version + "-aarch64-unknown-linux-gnu.tar.gz")},
            {name: ("herdr-agent-context-v" + $version + "-x86_64-unknown-linux-gnu.tar.gz")},
            {name: "SHA256SUMS"}
          ]
        }'
}

latest_json() {
    jq -n --arg tag "$TAG" '{tag_name: $tag}'
}

reset_fixture() {
    rm -f "$TMP/fixture"/*
    : >"$TMP/gh.log"
    printf '200\n' >"$TMP/fixture/tag.status"
    printf '200\n' >"$TMP/fixture/latest.status"
    exact_release_json >"$TMP/fixture/tag.json"
    latest_json >"$TMP/fixture/latest.json"
}

run_checker() {
    GH_COMMAND="$TMP/fake-gh" \
    FAKE_GH_LOG="$TMP/gh.log" \
    FAKE_GH_FIXTURE="$TMP/fixture" \
        "$SCRIPT" "$VERSION" "$TMP/notes.md" "$REPOSITORY"
}

expect_fail() {
    description=$1
    if run_checker >"$TMP/$description.out" 2>"$TMP/$description.err"; then
        echo "GitHub release test: $description unexpectedly passed" >&2
        exit 1
    fi
}

reset_fixture
printf '404\n' >"$TMP/fixture/tag.status"
printf '{"message":"Not Found"}\n' >"$TMP/fixture/tag.json"
test "$(run_checker)" = absent
test "$(wc -l <"$TMP/gh.log" | tr -d ' ')" -eq 1
grep -Fq "repos/$REPOSITORY/releases/tags/$TAG" "$TMP/gh.log"

reset_fixture
test "$(run_checker)" = existing
test "$(wc -l <"$TMP/gh.log" | tr -d ' ')" -eq 2
grep -Fq "repos/$REPOSITORY/releases/latest" "$TMP/gh.log"

for status in 401 403 500; do
    reset_fixture
    printf '%s\n' "$status" >"$TMP/fixture/tag.status"
    printf '{"message":"API failure"}\n' >"$TMP/fixture/tag.json"
    expect_fail "tag-api-$status"
done

reset_fixture
touch "$TMP/fixture/tag.transport"
expect_fail tag-transport

reset_fixture
printf '{malformed\n' >"$TMP/fixture/tag.json"
expect_fail malformed-tag-json

for filter in \
    '.draft = true' \
    '.prerelease = true' \
    '.tag_name = "v9.9.9"' \
    '.name = "wrong title"'; do
    reset_fixture
    jq "$filter" "$TMP/fixture/tag.json" >"$TMP/changed.json"
    mv "$TMP/changed.json" "$TMP/fixture/tag.json"
    expect_fail "release-state-$(printf '%s' "$filter" | tr -cd '[:alnum:]')"
done

reset_fixture
jq '.body += "changed without the required separator"' "$TMP/fixture/tag.json" >"$TMP/changed.json"
mv "$TMP/changed.json" "$TMP/fixture/tag.json"
expect_fail changed-body

reset_fixture
jq '.assets = .assets[0:4]' "$TMP/fixture/tag.json" >"$TMP/changed.json"
mv "$TMP/changed.json" "$TMP/fixture/tag.json"
expect_fail missing-asset

reset_fixture
jq '.assets += [{name: "unexpected.tar.gz"}]' "$TMP/fixture/tag.json" >"$TMP/changed.json"
mv "$TMP/changed.json" "$TMP/fixture/tag.json"
expect_fail extra-asset

reset_fixture
jq '.assets[4].name = .assets[0].name' "$TMP/fixture/tag.json" >"$TMP/changed.json"
mv "$TMP/changed.json" "$TMP/fixture/tag.json"
expect_fail duplicate-assets

reset_fixture
printf '500\n' >"$TMP/fixture/latest.status"
printf '{"message":"API failure"}\n' >"$TMP/fixture/latest.json"
expect_fail latest-api

reset_fixture
touch "$TMP/fixture/latest.transport"
expect_fail latest-transport

reset_fixture
printf '{malformed\n' >"$TMP/fixture/latest.json"
expect_fail malformed-latest-json

reset_fixture
jq '.tag_name = "v0.0.1"' "$TMP/fixture/latest.json" >"$TMP/changed.json"
mv "$TMP/changed.json" "$TMP/fixture/latest.json"
expect_fail non-latest

cp "$TMP/notes.md" "$TMP/invalid-notes.md"
printf 'invalid trailing text' >>"$TMP/invalid-notes.md"
if GH_COMMAND="$TMP/fake-gh" FAKE_GH_LOG="$TMP/gh.log" FAKE_GH_FIXTURE="$TMP/fixture" \
    "$SCRIPT" "$VERSION" "$TMP/invalid-notes.md" "$REPOSITORY" >/dev/null 2>&1; then
    echo "GitHub release test: invalid expected notes unexpectedly passed" >&2
    exit 1
fi

# CI exercises every nonpublishing release contract suite.
for test_script in release-notes prepare-release release-tag github-release installer release-assets; do
    grep -Eq "sh tests/$test_script\\.sh" "$CI_WORKFLOW" || {
        echo "GitHub release test: CI does not run tests/$test_script.sh" >&2
        exit 1
    }
done

# Publication delegates all lookup and decision logic to the tested checker.
grep -Fq 'sh scripts/check-github-release.sh' "$WORKFLOW"
grep -Fq "case \"\$decision\" in" "$WORKFLOW"
grep -Fq 'absent)' "$WORKFLOW"
grep -Fq 'existing)' "$WORKFLOW"
if grep -Eq 'gh (api|release view)' "$WORKFLOW"; then
    echo "GitHub release test: workflow reimplements release lookup" >&2
    exit 1
fi

# Stable validation and rendered notes are prerequisites of every target build.
grep -Fq "sh scripts/validate-release-tag.sh \"\$GITHUB_REF_NAME\" \"\$GITHUB_SHA\"" "$WORKFLOW"
grep -Fq "sh scripts/release-notes.sh render \"\$version\" >release-notes.md" "$WORKFLOW"
grep -Eq '^  build:' "$WORKFLOW"
grep -Eq '^    needs: validate$' "$WORKFLOW"

if grep -Eq 'prerelease: true|generate_release_notes: true|--prerelease' "$WORKFLOW"; then
    echo "GitHub release test: prerelease or generated-note publication remains enabled" >&2
    exit 1
fi

printf 'GitHub release tests passed\n'
