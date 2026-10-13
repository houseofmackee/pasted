#!/bin/sh
# Prints the CHANGELOG.md section for a version (its body, without the
# heading), as Markdown, or as HTML with --html. Fails when the version has
# no section or the section is empty, so a release cannot ship without
# notes.
#
#   scripts/release_notes.sh 0.3.2
#   scripts/release_notes.sh 0.3.2 --html   (needs target/release/pasted)
set -eu
cd "$(dirname "$0")/.."

VERSION=${1:?usage: release_notes.sh VERSION [--html]}
SECTION=$(awk -v heading="## [$VERSION]" '
    index($0, heading) == 1 { found = 1; next }
    found && /^## \[/ { exit }
    found && /^\[[^]]*\]: / { exit }
    found { print }
' CHANGELOG.md | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}')

if [ -z "$(printf '%s' "$SECTION" | tr -d '[:space:]')" ]; then
    echo "error: CHANGELOG.md has no notes for $VERSION (add a \"## [$VERSION] - YYYY-MM-DD\" section)" >&2
    exit 1
fi

if [ "${2:-}" = "--html" ]; then
    # Pasted's own Markdown renderer (the "Formatted Markdown" Paste As).
    printf '%s\n' "$SECTION" | target/release/pasted transform markdown
else
    printf '%s\n' "$SECTION"
fi
