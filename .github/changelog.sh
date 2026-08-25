#!/usr/bin/env bash
# Build a changelog for one release, from the commit subjects between two
# tags.
#
# The subjects here are prose, not conventional commits - "netwatch: every
# rate read a quarter too high" rather than "fix(netwatch): ...". There is no
# type to group by and inventing one would mean rewriting three hundred
# subjects. What every subject does carry is the part before the first colon,
# which is the widget or the area it touched, so that is the grouping: a
# reader asking "what changed in ports" gets an answer, which is the question
# a changelog for fourteen separate binaries is actually asked.
#
# A prefix only counts as a scope if it names something real - a widget in
# widgets/src/bin, or one of the areas below. Early history has subjects like
# "Add agents.py: every coding agent", where the part before the colon is the
# start of a sentence rather than a scope; taken at face value those made
# forty-seven sections out of three hundred commits. The widget list is read
# from the tree rather than written down here, so a new widget is a scope the
# day it lands.
#
# Usage: changelog.sh <tag> [previous-tag]
# With no previous tag it takes the whole history, which is what the first
# release needs and what it will get, there being no tags yet.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tag="${1:?usage: changelog.sh <tag> [previous-tag]}"
prev="${2:-}"

if [ -z "$prev" ]; then
    prev=$(git describe --tags --abbrev=0 "${tag}^" 2>/dev/null || true)
fi
if [ -n "$prev" ]; then
    range="${prev}..${tag}"
    since="since ${prev}"
else
    range="$tag"
    since="everything so far"
fi

# Every widget, plus the places that are not one.
scopes=$(
    { ls "$here/widgets/src/bin" 2>/dev/null | sed -n 's/\.rs$//p'
      printf '%s\n' core widgets docs tests ci rust release
    } | sort -u
)

printf '## %s\n\n' "$tag"
printf '%s commits, %s.\n\n' "$(git log --oneline "$range" | wc -l | tr -d ' ')" "$since"

git log --format='%s' "$range" | awk -v known="$scopes" -F': ' '
    BEGIN { n = split(known, k, "\n"); for (i = 1; i <= n; i++) if (k[i] != "") is[k[i]] = 1 }
    {
        if (NF < 2) { print "everything else\t" $0; next }
        scope = $1
        rest  = substr($0, length(scope) + 3)
        n = split(scope, parts, /, */)
        # Every part has to be a real scope. "linear, netwatch" is two
        # widgets; "Add agents.py" is a sentence that happens to contain a
        # comma-free prefix and belongs whole, under everything else.
        good = (n > 0)
        for (i = 1; i <= n; i++) if (!(parts[i] in is)) good = 0
        if (!good) { print "everything else\t" $0; next }
        for (i = 1; i <= n; i++) print parts[i] "\t" rest
    }
' | sort -f -t"$(printf '\t')" -k1,1 | awk -F'\t' '
    $1 != seen { if (seen != "") printf "\n"; printf "### %s\n\n", $1; seen = $1 }
    { printf "- %s\n", $2 }
'
