#!/bin/sh
set -eu

ROOT=${HERDR_AGENT_CONTEXT_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
CHANGELOG="$ROOT/CHANGELOG.md"

usage() {
    echo "usage: release-notes.sh <check [X.Y.Z] | render X.Y.Z | verify X.Y.Z BODY_FILE>" >&2
    exit 2
}

package_version() {
    awk '
        /^\[package\]$/ { package = 1; next }
        /^\[/ { package = 0 }
        package && /^version = "/ {
            value = $0
            sub(/^version = "/, "", value)
            sub(/"$/, "", value)
            print value
            exit
        }
    ' "$ROOT/Cargo.toml"
}

validate_changelog() {
    target=$1
    awk -v target="$target" '
        function fail(message) {
            print "CHANGELOG.md: " message > "/dev/stderr"
            failed = 1
            exit 1
        }
        function valid_part(value) {
            return value ~ /^(0|[1-9][0-9]*)$/
        }
        function valid_version(value, parts) {
            return split(value, parts, ".") == 3 &&
                valid_part(parts[1]) && valid_part(parts[2]) && valid_part(parts[3])
        }
        function compare_versions(left, right, left_parts, right_parts, i) {
            split(left, left_parts, ".")
            split(right, right_parts, ".")
            for (i = 1; i <= 3; i++) {
                if ((left_parts[i] + 0) > (right_parts[i] + 0)) return 1
                if ((left_parts[i] + 0) < (right_parts[i] + 0)) return -1
            }
            return 0
        }
        function finish_category() {
            if (category != "" && !category_has_bullet) {
                fail("version " version " category " category " requires at least one bullet")
            }
            category = ""
            category_has_bullet = 0
        }
        function finish_section() {
            finish_category()
            if (version != "" && !section_has_category) {
                fail("version " version " requires at least one category")
            }
        }
        function strip_comments(line, start, finish, before, after) {
            while (1) {
                if (in_comment) {
                    finish = index(line, "-->")
                    if (!finish) return ""
                    line = substr(line, finish + 3)
                    in_comment = 0
                    continue
                }
                start = index(line, "<!--")
                if (!start) return line
                before = substr(line, 1, start - 1)
                after = substr(line, start + 4)
                finish = index(after, "-->")
                if (finish) {
                    line = before substr(after, finish + 3)
                    continue
                }
                in_comment = 1
                return before
            }
        }
        BEGIN {
            allowed["Added"] = allowed["Changed"] = allowed["Deprecated"] = 1
            allowed["Removed"] = allowed["Fixed"] = allowed["Security"] = 1
        }
        {
            if (in_fence) {
                line = $0
                if (line ~ /^ {0,3}(```+|~~~+)/) {
                    marker = substr(line, match(line, /(```+|~~~+)/), RLENGTH)
                    character = substr(marker, 1, 1)
                    if (character == fence_character && length(marker) >= fence_length) {
                        in_fence = 0
                    }
                }
                next
            }

            line = strip_comments($0)
            if (line ~ /^ {0,3}(```+|~~~+)/) {
                marker = substr(line, match(line, /(```+|~~~+)/), RLENGTH)
                in_fence = 1
                fence_character = substr(marker, 1, 1)
                fence_length = length(marker)
                next
            }
            if (line ~ /^[[:space:]]*$/) next

            if (line ~ /^## /) {
                if (line !~ /^## v/) fail("invalid release heading " substr(line, 4))
                candidate = substr(line, 5)
                if (!valid_version(candidate)) fail("invalid release heading v" candidate)
                finish_section()
                if (seen_version[candidate]) fail("duplicate changelog version " candidate)
                if (version != "" && compare_versions(version, candidate) <= 0) {
                    fail("release versions must be strictly descending")
                }
                seen_version[candidate] = 1
                version = candidate
                if (latest == "") latest = candidate
                if (candidate == target) target_found = 1
                section_has_category = 0
                category = ""
                next
            }
            if (line ~ /^### /) {
                if (version == "") fail("category appears before a release heading")
                finish_category()
                category = substr(line, 5)
                if (!allowed[category]) fail("version " version " has unknown category " category)
                key = version SUBSEP category
                if (seen_category[key]) fail("version " version " has duplicate category " category)
                seen_category[key] = 1
                section_has_category = 1
                category_has_bullet = 0
                next
            }
            if (version == "") next
            if (!section_has_category) {
                if (line !~ /^_[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]_$/) {
                    fail("version " version " has invalid text before its categories")
                }
                next
            }
            if (category != "" && line ~ /^- [^[:space:]]/) category_has_bullet = 1
        }
        END {
            if (!failed) {
                finish_section()
                if (latest == "") fail("requires at least one version section")
                if (!target_found) fail("target version " target " was not found")
                if (latest != target) {
                    fail("target version " target " does not match latest changelog version " latest)
                }
            }
        }
    ' "$CHANGELOG"
}

release_body() {
    target=$1
    awk -v target="$target" '
        function strip_comments(line, start, finish, before, after) {
            while (1) {
                if (in_comment) {
                    finish = index(line, "-->")
                    if (!finish) return ""
                    line = substr(line, finish + 3)
                    in_comment = 0
                    continue
                }
                start = index(line, "<!--")
                if (!start) return line
                before = substr(line, 1, start - 1)
                after = substr(line, start + 4)
                finish = index(after, "-->")
                if (finish) {
                    line = before substr(after, finish + 3)
                    continue
                }
                in_comment = 1
                return before
            }
        }
        function emit(line) {
            if (!started && line ~ /^[[:space:]]*$/) return
            if (line ~ /^[[:space:]]*$/) {
                pending = pending "\n"
                return
            }
            if (started) printf "%s", pending
            print line
            started = 1
            pending = ""
        }
        {
            raw = $0
            if (in_fence) {
                if (raw ~ /^ {0,3}(```+|~~~+)/) {
                    marker = substr(raw, match(raw, /(```+|~~~+)/), RLENGTH)
                    character = substr(marker, 1, 1)
                    if (character == fence_character && length(marker) >= fence_length) {
                        in_fence = 0
                    }
                }
                if (found) emit(raw)
                next
            }

            visible = strip_comments(raw)
            if (visible ~ /^ {0,3}(```+|~~~+)/) {
                marker = substr(visible, match(visible, /(```+|~~~+)/), RLENGTH)
                in_fence = 1
                fence_character = substr(marker, 1, 1)
                fence_length = length(marker)
                if (found) emit(raw)
                next
            }
            if (visible ~ /^## v/) {
                version = substr(visible, 5)
                if (found) exit
                if (version == target) found = 1
                next
            }
            if (found) emit(raw)
        }
    ' "$CHANGELOG"
}

previous_version() {
    target=$1
    awk -v target="$target" '
        function strip_comments(line, start, finish, before, after) {
            while (1) {
                if (in_comment) {
                    finish = index(line, "-->")
                    if (!finish) return ""
                    line = substr(line, finish + 3)
                    in_comment = 0
                    continue
                }
                start = index(line, "<!--")
                if (!start) return line
                before = substr(line, 1, start - 1)
                after = substr(line, start + 4)
                finish = index(after, "-->")
                if (finish) {
                    line = before substr(after, finish + 3)
                    continue
                }
                in_comment = 1
                return before
            }
        }
        {
            if (in_fence) {
                line = $0
                if (line ~ /^ {0,3}(```+|~~~+)/) {
                    marker = substr(line, match(line, /(```+|~~~+)/), RLENGTH)
                    character = substr(marker, 1, 1)
                    if (character == fence_character && length(marker) >= fence_length) {
                        in_fence = 0
                    }
                }
                next
            }

            line = strip_comments($0)
            if (line ~ /^ {0,3}(```+|~~~+)/) {
                marker = substr(line, match(line, /(```+|~~~+)/), RLENGTH)
                in_fence = 1
                fence_character = substr(marker, 1, 1)
                fence_length = length(marker)
                next
            }
            if (line ~ /^## v/) {
                version = substr(line, 5)
                if (seen_target) {
                    print version
                    exit
                }
                if (version == target) seen_target = 1
            }
        }
    ' "$CHANGELOG"
}

render_notes() {
    version=$1
    validate_changelog "$version"
    previous=$(previous_version "$version")
    if [ -z "$previous" ]; then
        echo "CHANGELOG.md: version $version cannot be rendered without a next-older changelog entry" >&2
        return 1
    fi
    body=$(release_body "$version")
    cat <<EOF
# herdr-agent-context v$version

## Release Notes

$body

## Install

\`\`\`sh
herdr plugin install ryonakae/herdr-agent-context --ref v$version --yes
\`\`\`

## Validation

- Repository tests, formatting, linting, and release build checks passed.
- All four release archives passed checksum, content, executable, and Linux compatibility checks.
- The release installer installed a binary byte-identical to its archive.

## Full changelog

https://github.com/ryonakae/herdr-agent-context/compare/v$previous...v$version
EOF
}

verify_notes() {
    version=$1
    body_file=$2
    [ -f "$body_file" ] || { echo "release body not found: $body_file" >&2; return 1; }
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/herdr-agent-context-notes-verify.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT HUP INT TERM
    render_notes "$version" >"$tmp/expected"
    expected_size=$(wc -c <"$tmp/expected" | tr -d ' ')
    actual_size=$(wc -c <"$body_file" | tr -d ' ')
    if [ "$actual_size" -lt "$expected_size" ]; then
        echo "GitHub Release v$version: missing or altered required release content" >&2
        return 1
    fi
    dd if="$body_file" of="$tmp/prefix" bs=1 count="$expected_size" 2>/dev/null
    if ! cmp -s "$tmp/expected" "$tmp/prefix"; then
        echo "GitHub Release v$version: missing or altered required release content" >&2
        return 1
    fi
    if [ "$actual_size" -gt "$expected_size" ]; then
        dd if="$body_file" of="$tmp/trailing" bs=1 skip="$expected_size" 2>/dev/null
        first=$(dd if="$tmp/trailing" bs=1 count=1 2>/dev/null || true)
        if [ -n "$first" ] || awk -v title="herdr-agent-context v$version" '
            function strip_comments(line, start, finish, before, after) {
                while (1) {
                    if (in_comment) {
                        finish = index(line, "-->")
                        if (!finish) return ""
                        line = substr(line, finish + 3)
                        in_comment = 0
                        continue
                    }
                    start = index(line, "<!--")
                    if (!start) return line
                    before = substr(line, 1, start - 1)
                    after = substr(line, start + 4)
                    finish = index(after, "-->")
                    if (finish) {
                        line = before substr(after, finish + 3)
                        continue
                    }
                    in_comment = 1
                    return before
                }
            }
            function reserved_heading(line, indentation, content, level, i) {
                match(line, /^ */)
                indentation = RLENGTH
                if (indentation > 3) return 0
                line = substr(line, indentation + 1)
                if (substr(line, 1, 1) != "#") return 0
                level = 0
                for (i = 1; substr(line, i, 1) == "#"; i++) level += 1
                if (level > 6) return 0
                if (substr(line, level + 1, 1) !~ /[[:space:]]/ && length(line) > level) return 0
                content = substr(line, level + 1)
                sub(/^[[:space:]]+/, "", content)
                sub(/[[:space:]]+$/, "", content)
                sub(/[[:space:]]+#+$/, "", content)
                sub(/[[:space:]]+$/, "", content)
                return (level == 1 && content == title) ||
                    (level == 2 && (content == "Release Notes" || content == "Install" ||
                    content == "Validation" || content == "Full changelog"))
            }
            {
                if (in_fence) {
                    line = $0
                    if (line ~ /^ {0,3}(```+|~~~+)/) {
                        marker = substr(line, match(line, /(```+|~~~+)/), RLENGTH)
                        character = substr(marker, 1, 1)
                        if (character == fence_character && length(marker) >= fence_length) {
                            in_fence = 0
                        }
                    }
                    next
                }
                line = strip_comments($0)
                if (line ~ /^ {0,3}(```+|~~~+)/) {
                    marker = substr(line, match(line, /(```+|~~~+)/), RLENGTH)
                    in_fence = 1
                    fence_character = substr(marker, 1, 1)
                    fence_length = length(marker)
                    next
                }
                if (reserved_heading(line)) found = 1
            }
            END { exit !found }
        ' "$tmp/trailing"; then
            echo "GitHub Release v$version: invalid operator notes after required release content" >&2
            return 1
        fi
    fi
    printf '%s\n' "$version"
}

command=${1:-}
case "$command" in
    check)
        if [ "$#" -gt 2 ]; then usage; fi
        version=${2:-$(package_version)}
        [ -n "$version" ] || { echo "Cargo.toml: could not read package version" >&2; exit 1; }
        validate_changelog "$version"
        printf '%s\n' "$version"
        ;;
    render)
        [ "$#" -eq 2 ] || usage
        render_notes "$2"
        ;;
    verify)
        [ "$#" -eq 3 ] || usage
        verify_notes "$2" "$3"
        ;;
    *)
        usage
        ;;
esac
