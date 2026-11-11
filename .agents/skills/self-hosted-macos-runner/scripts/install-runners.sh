#!/usr/bin/env bash
# Install N GitHub Actions runner instances on this Mac, each as a LaunchAgent.
#
# Usage:
#   read -rs RUNNER_TOKEN && export RUNNER_TOKEN   # paste; stays out of history
#   ./install-runners.sh \
#     --url https://github.com/<org> --group <runner group> \
#     --labels <mac-label>,xcode-<major> [--count 2] [--root ~/actions-runner] \
#     [--replace]
#
# The token comes from Org → Settings → Actions → Runners → New self-hosted
# runner, or POST /orgs/{org}/actions/runners/registration-token. It is read
# from the environment and handed to config.sh as ACTIONS_RUNNER_INPUT_TOKEN,
# so it never appears in a process list. Re-running is safe: an instance that is already
# configured keeps its registration and only has its service (re)started; new
# instances (a higher --count) are registered.
set -euo pipefail

url="" group="" labels="" count=2 root="$HOME/actions-runner" replace=""
while [ $# -gt 0 ]; do
  case "$1" in
    --url) url="$2"; shift 2 ;;
    --group) group="$2"; shift 2 ;;
    --labels) labels="$2"; shift 2 ;;
    --count) count="$2"; shift 2 ;;
    --root) root="$2"; shift 2 ;;
    # Take over a registration of the same name (e.g. after reinstalling into
    # a new --root). Off by default: it takes the existing runner offline.
    --replace) replace=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
: "${RUNNER_TOKEN:?set RUNNER_TOKEN to a registration token}"
[ -n "$url" ] || { echo "--url is required" >&2; exit 2; }
# No silent Default: that group is usually shared with every repo in the org,
# and the group is what limits which repos can run code on this Mac.
[ -n "$group" ] || { echo "--group is required (a runner group limited to selected private repos)" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] || { echo "run this on the Mac" >&2; exit 1; }
[ "$(uname -m)" = arm64 ] || { echo "this script installs the arm64 runner" >&2; exit 1; }

asset=$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest | python3 -c '
import json, sys
for a in json.load(sys.stdin)["assets"]:
    n = a["name"]
    if n.startswith("actions-runner-osx-arm64-") and n.endswith(".tar.gz"):
        print(a["browser_download_url"]); break
')
[ -n "$asset" ] || { echo "could not find the macOS arm64 runner download" >&2; exit 1; }
mkdir -p "$root"
root=$(cd "$root" && pwd)   # absolute: the loop below cds into each instance
tarball="$root/$(basename "$asset")"
if [ ! -f "$tarball" ]; then
  # Download beside the target and rename on success, so a failed download
  # never leaves a partial archive that a later run would trust.
  curl -fsSL -o "$tarball.part" "$asset"
  mv "$tarball.part" "$tarball"
fi

host=$(scutil --get LocalHostName)
for i in $(seq 1 "$count"); do
  dir="$root/$i"
  mkdir -p "$dir"
  cd "$dir"
  if [ -f .runner ]; then
    # config.sh refuses to run on a configured instance, even with --replace.
    # Keep the registration and just make sure its service is running. To
    # re-register (new group, labels or URL), run `./config.sh remove --token
    # <removal token>` in this directory first, then re-run this script.
    # The existing registration keeps the group and labels it was created
    # with; --group and --labels do not apply to it. If GitHub no longer lists
    # it (runners offline for 14 days are removed), the service will start and
    # never take a job: run `./config.sh remove --token <removal token>` here
    # (or delete .runner) and re-run this script.
    ./svc.sh install >/dev/null 2>&1 || true
    ./svc.sh start
    echo "instance $i: already registered (keeps its original group and labels), service started"
    continue
  fi
  [ -x ./config.sh ] || tar xzf "$tarball"
  ACTIONS_RUNNER_INPUT_TOKEN="$RUNNER_TOKEN" ./config.sh --unattended \
    ${replace:+--replace} --url "$url" \
    --name "$host-$i" --runnergroup "$group" \
    ${labels:+--labels "$labels"} --work _work
  ./svc.sh install
  ./svc.sh start
  echo "instance $i: $host-$i registered and started"
done

echo
echo "Check: Org → Settings → Actions → Runners should list $count idle runner(s)"
echo "in group $group. An instance that was already registered is not re-checked here."
echo "Simulators need a logged-in GUI session: turn on auto-login for $(whoami)."
