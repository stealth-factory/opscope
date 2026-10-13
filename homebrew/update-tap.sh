#!/usr/bin/env bash
set -euo pipefail

# GH_TOKEN must have Contents write access to the tap. Never put it in a URL.
version="${1:?Usage: bash homebrew/update-tap.sh VERSION ARTIFACTS}"
artifacts="${2:?An artefact directory is required}"
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
endpoint=repos/stealth-factory/homebrew-tap/contents/Formula/opscope.rb

# Contents API updates only this file, so another application's tap release
# is preserved. The file SHA provides compare-and-swap protection; retry by
# reading the latest file, including its version, rather than force-pushing.
for attempt in 1 2 3; do
  if ! gh api "$endpoint?ref=main" > "$work/current.json"; then
    echo "Cannot read Formula/opscope.rb on the tap's main branch." >&2
    echo "Merge the initial formula PR before running release updates; also verify the existing bot can access the tap." >&2
    exit 1
  fi
  jq -r .content "$work/current.json" | base64 --decode > "$work/current.rb"
  rm -f "$work/opscope.rb"
  node "$script_dir/formula.js" "$version" "$artifacts" "$work/opscope.rb" "$work/current.rb"
  if [ ! -f "$work/opscope.rb" ] || cmp -s "$work/current.rb" "$work/opscope.rb"; then
    echo "Homebrew formula already matches or exceeds $version"
    exit 0
  fi
  jq --rawfile formula "$work/opscope.rb" --arg version "$version" \
    '{message: ("chore: update opscope to " + $version), branch: "main",
      sha: .sha, content: ($formula | @base64)}' \
    "$work/current.json" > "$work/update.json"
  if gh api --method PUT "$endpoint" --input "$work/update.json" --silent; then
    echo "Updated Homebrew formula to $version"
    exit 0
  fi
  echo "Tap update failed (attempt $attempt); reading it again" >&2
done
echo "Homebrew tap update failed after three attempts" >&2
exit 1
