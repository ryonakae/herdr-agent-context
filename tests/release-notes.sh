#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT="$ROOT/scripts/release-notes.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-release-notes-test.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

new_root() {
    name=$1
    version=${2:-0.4.0}
    directory="$TMP/$name"
    mkdir -p "$directory"
    cat >"$directory/Cargo.toml" <<EOF
[package]
name = "herdr-agent-context"
version = "$version"
EOF
    cat >"$directory/CHANGELOG.md"
}

expect_fail() {
    description=$1
    shift
    if "$@" >"$TMP/failure.out" 2>"$TMP/failure.err"; then
        echo "release notes test: $description unexpectedly passed" >&2
        exit 1
    fi
}

new_root valid <<'EOF'
# Changelog

## v0.4.0

_2026-08-30_

### Added

- Added Codex context.

```markdown
## v99.0.0
### Unknown
- Hidden in a fence.
```

<!--
## v98.0.0
### Unknown
- Hidden in a comment.
-->

## v0.3.0

### Fixed

- Fixed naming recovery.
EOF

result=$(HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" check 0.4.0)
test "$result" = "0.4.0"
test "$(HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" check)" = "0.4.0"

new_root missing <<'EOF'
# Changelog
## v0.3.0
### Added
- Older release.
EOF
expect_fail "missing target" env HERDR_AGENT_CONTEXT_ROOT="$TMP/missing" "$SCRIPT" check 0.4.0

new_root non-latest <<'EOF'
# Changelog
## v0.5.0
### Added
- New release.
## v0.4.0
### Added
- Old release.
EOF
expect_fail "non-latest target" env HERDR_AGENT_CONTEXT_ROOT="$TMP/non-latest" "$SCRIPT" check 0.4.0

new_root duplicate <<'EOF'
# Changelog
## v0.4.0
### Added
- First.
## v0.4.0
### Fixed
- Duplicate.
EOF
expect_fail "duplicate version" env HERDR_AGENT_CONTEXT_ROOT="$TMP/duplicate" "$SCRIPT" check 0.4.0

new_root ascending <<'EOF'
# Changelog
## v0.4.0
### Added
- First.
## v0.5.0
### Added
- Ascending.
EOF
expect_fail "ascending versions" env HERDR_AGENT_CONTEXT_ROOT="$TMP/ascending" "$SCRIPT" check 0.4.0

new_root invalid-version <<'EOF'
# Changelog
## v0.4
### Added
- Invalid.
EOF
expect_fail "invalid version" env HERDR_AGENT_CONTEXT_ROOT="$TMP/invalid-version" "$SCRIPT" check 0.4.0

new_root unknown-category <<'EOF'
# Changelog
## v0.4.0
### Improved
- Unknown.
EOF
expect_fail "unknown category" env HERDR_AGENT_CONTEXT_ROOT="$TMP/unknown-category" "$SCRIPT" check 0.4.0

new_root duplicate-category <<'EOF'
# Changelog
## v0.4.0
### Added
- First.
### Added
- Duplicate.
EOF
expect_fail "duplicate category" env HERDR_AGENT_CONTEXT_ROOT="$TMP/duplicate-category" "$SCRIPT" check 0.4.0

new_root empty-category <<'EOF'
# Changelog
## v0.4.0
### Added
Text without a bullet.
EOF
expect_fail "category without bullet" env HERDR_AGENT_CONTEXT_ROOT="$TMP/empty-category" "$SCRIPT" check 0.4.0

new_root invalid-preamble <<'EOF'
# Changelog
## v0.4.0
Summary text is not allowed here.
### Added
- Entry.
EOF
expect_fail "invalid preamble" env HERDR_AGENT_CONTEXT_ROOT="$TMP/invalid-preamble" "$SCRIPT" check 0.4.0

HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" render 0.4.0 >"$TMP/rendered.md"
cat >"$TMP/expected.md" <<'EOF'
# herdr-agent-context v0.4.0

## Release Notes

_2026-08-30_

### Added

- Added Codex context.

```markdown
## v99.0.0
### Unknown
- Hidden in a fence.
```

<!--
## v98.0.0
### Unknown
- Hidden in a comment.
-->

## Install

```sh
herdr plugin install ryonakae/herdr-agent-context --ref v0.4.0 --yes
```

## Validation

- Repository tests, formatting, linting, and release build checks passed.
- All four release archives passed checksum, content, executable, and Linux compatibility checks.
- The release installer installed a binary byte-identical to its archive.

## Full changelog

https://github.com/ryonakae/herdr-agent-context/compare/v0.3.0...v0.4.0
EOF
cmp "$TMP/expected.md" "$TMP/rendered.md"

test "$(HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" verify 0.4.0 "$TMP/rendered.md")" = "0.4.0"
printf '\nOperator note.\n' >>"$TMP/rendered.md"
test "$(HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" verify 0.4.0 "$TMP/rendered.md")" = "0.4.0"

cp "$TMP/expected.md" "$TMP/prefixed.md"
printf 'Prefix.\n%s' "$(cat "$TMP/prefixed.md")" >"$TMP/prefixed.md"
expect_fail "prefixed body" env HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" verify 0.4.0 "$TMP/prefixed.md"

awk '/^## Install$/ { print "Inserted text." } { print }' "$TMP/expected.md" >"$TMP/inserted.md"
expect_fail "text between blocks" env HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" verify 0.4.0 "$TMP/inserted.md"

cat "$TMP/expected.md" >"$TMP/duplicated.md"
printf '\n## Install\n\nDuplicate.\n' >>"$TMP/duplicated.md"
expect_fail "duplicated mandatory block" env HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" verify 0.4.0 "$TMP/duplicated.md"

sed 's/## Validation/## Checks/' "$TMP/expected.md" >"$TMP/changed.md"
expect_fail "changed mandatory block" env HERDR_AGENT_CONTEXT_ROOT="$TMP/valid" "$SCRIPT" verify 0.4.0 "$TMP/changed.md"

printf 'release notes tests passed\n'
