#!/bin/sh
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: verify-glibc-baseline.sh BINARY [MAX_VERSION]" >&2
    exit 2
fi

BINARY=$1
MAX_VERSION=${2:-2.17}
test -f "$BINARY" || {
    echo "herdr-agent-context: binary is missing: $BINARY" >&2
    exit 1
}
command -v strings >/dev/null 2>&1 || {
    echo "herdr-agent-context: strings is required" >&2
    exit 1
}

HIGHEST=$(strings "$BINARY" | grep -Eo 'GLIBC_[0-9]+\.[0-9]+' | sed 's/^GLIBC_//' | sort -Vu | tail -n 1)
test -n "$HIGHEST" || {
    echo "herdr-agent-context: no GLIBC symbol versions found" >&2
    exit 1
}

version_le() {
    awk -v actual="$1" -v maximum="$2" 'BEGIN {
        split(actual, a, "."); split(maximum, m, ".");
        exit !((a[1] < m[1]) || (a[1] == m[1] && a[2] <= m[2]));
    }'
}

version_le "$HIGHEST" "$MAX_VERSION" || {
    echo "herdr-agent-context: GLIBC $HIGHEST exceeds baseline $MAX_VERSION" >&2
    exit 1
}
printf 'herdr-agent-context: GLIBC baseline satisfied (%s <= %s)\n' "$HIGHEST" "$MAX_VERSION"
