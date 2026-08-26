# opscope

**Sci-fi hacker opscope from the movies — except these tell you the
truth.**

Fill your screen with glowing panels, scrolling graphs and blinking status
readouts. The difference is that every number on them is real: actual network
latency, actual deployments, actual machines on your tailnet, actual agents
waiting on you.

[![opscope — click to watch the 26-second demo](docs/demo-poster.jpg)](docs/demo.mp4)

*Seven widgets running at once — every figure live. [Watch the 26s demo →](docs/demo.mp4)*

## How it started

Literally: *"split 10 panels that make my computer look like some sci-fi hacker
terminal from the movies."*

So there were ten. A radar sweep, a rotating globe, a spectrum analyser, a
cipher cracker, a memory scanner, a packet intercept — all beautiful, all
fabricated. They looked exactly right and told you nothing.

Then one got wired to `/proc` and became genuinely useful, and the contrast was
impossible to unsee. One by one the fakes came down, each replaced by something
that answers a real question. What survived is the aesthetic with the lying
removed.

Some of the theatre is still here, unapologetically. `matrix` computes
nothing at all — it just looks good, and it knows it.

## The widgets

| Widget | What it does | Needs | Docs |
|---|---|---|---|
| **`opscope`** | The front door: every widget, what it does, and whether it will work on this machine — pick one and it runs, quit it and you are back. | — | [read →](docs/opscope.md) |
| **`latency`** | Continuous latency to a list of hosts: median, jitter, loss and a log-scale graph, so a slow link and an *unsteady* one look different. | `ping` | [read →](docs/latency.md) |
| **`deployments`** | Vercel deployments over time — activity per hour, build-time drift, and the build log of the one you open, so a failure explains itself instead of only naming a code. A copy page carries the dashboard, preview and PR URLs. | `curl`, a Vercel token | [read →](docs/deployments.md) |
| **`tailnet`** | Tailscale peers, and whether each is reached directly or through a relay. Live throughput, full machine info, copyable addresses. | `tailscale` | [read →](docs/tailnet.md) |
| **`herdr-panes`** | Every agent and process across all workspaces, ordered by which one needs a human. Enter jumps you there. | `herdr` | [read →](docs/herdr-panes.md) |
| **`github`** | Pull requests across every org: merge rate, opened-vs-merged per day, review backlog and the contribution calendar — and `↵` for one account on a screen of its own, because a queue growing in one of them is invisible in a total the others are also feeding. | `curl`, a GitHub token | [read →](docs/github.md) |
| **`pr`** | The pull requests you have to follow up on: checks, reviews, mergeability, and a stack map with the order a stack has to merge in. | `curl`, a GitHub token | [read →](docs/pr.md) |
| **`linear`** | Linear across every team: what is outstanding, the running cycles and their scope creep, issues created against completed, and every project still going. `↵` opens a cycle, a team or a project on a screen of its own. | `curl`, a Linear API key | [read →](docs/linear.md) |
| **`usage`** | How much each coding agent on the machine has been used — tokens, sessions, AI-written code — and what is left of each one's rate limit, one tab per agent. | the agents' own logins | [read →](docs/usage.md) |
| **`ports`** | What is listening on this machine — the dev servers you have running, which project each was started from, how long it has been up, how much traffic it is carrying, and whether anything outside the box can reach it. `k` stops the selected one; `↵` opens it for a traffic chart, an address to copy, or a publish over Tailscale or Cloudflare. | `ss` for the traffic | [read →](docs/ports.md) |
| **`netwatch`** | Which processes are using the network — total since it started, current rate, up and down, per process — read from the kernel's own per-socket counters rather than by capturing packets. | `ss` | [read →](docs/netwatch.md) |
| **`link`** | How good the connection is between this machine and whoever is connected to it — round-trip time, jitter, loss and achieved rate for every inbound session, read from the kernel rather than probed. | `ss` | [read →](docs/link.md) |
| **`clocks`** | Server clock, countdowns to the next hour / end of office hours / end of day, a pomodoro, and a world clock. | — | [read →](docs/clocks.md) |
| **`matrix`** | Nothing whatsoever. Digital rain, with truecolor fade trails. | — | — |

Each is a single self-contained binary — every library it needs is compiled
in, `ldd` shows only libc, libm and libgcc, and there is nothing to install
alongside it. 24-bit colour, and a full redraw each frame so everything
reflows when you resize the pane.

## Getting them

There is no installer and nothing to package-manage yet — no `npx`, no
`brew`. You download three files, or you build fourteen. Both take about a
minute.

### Download a release

Linux x86-64, macOS Apple Silicon and macOS Intel are built for every tag.
This works out which one you want, checks it against its own checksum, and
unpacks it — no version to fill in, because it asks which release is
current:

```sh
R=https://github.com/stealth-factory/terminal-toys/releases
V=$(curl -fsSLI -o /dev/null -w '%{url_effective}' $R/latest | sed 's|.*/||')
case "$(uname -s) $(uname -m)" in
  "Darwin arm64")   A=aarch64-apple-darwin ;;
  "Darwin x86_64")  A=x86_64-apple-darwin ;;
  "Linux x86_64")   A=x86_64-unknown-linux-gnu ;;
  *) echo "no build for $(uname -sm); build from source below" >&2 ;;
esac
T=opscope-$V-$A
curl -fsSLO $R/download/$V/$T.tar.gz
curl -fsSLO $R/download/$V/$T.tar.gz.sha256
shasum -a 256 -c $T.tar.gz.sha256    # sha256sum -c on Linux; either is fine
tar -xzf $T.tar.gz && cd $T
```

Or take them by hand from the
[latest release](https://github.com/stealth-factory/terminal-toys/releases/latest)
— every tarball has a `.sha256` beside it.

The fourteen binaries are right there, beside `config.example.json` and a
copy of the docs. Nothing else is needed to run them, so this folder can
live wherever you like — or put the binaries on your `PATH`:

```sh
sudo cp opscope clocks deployments github herdr-panes latency linear link \
        matrix netwatch ports pr tailnet usage /usr/local/bin/
```

**On macOS, download with `curl` rather than a browser.** These binaries are
not signed or notarised, and a browser marks what it downloads with
`com.apple.quarantine`, which makes Gatekeeper refuse to run them. `curl`
does not set that mark. If you did use a browser, clear it:

```sh
xattr -d com.apple.quarantine ./*
```

*(Written from how Gatekeeper is documented to behave — this repo has no Mac
to check it on. If it is wrong, that is worth an issue.)*

### Or build them

Needs a Rust toolchain and nothing else:

```sh
cargo build --release   # fourteen binaries in ./target/release
```

## Running them

`opscope` is the front door — a menu of the fourteen, with a live preview of
whichever is highlighted:

```sh
opscope          # pick one and it runs
opscope latency  # or name one and skip the menu
```

Each widget is also an ordinary program, if you would rather go direct:

```sh
latency        # each runs standalone
clocks -h      # every widget documents itself
```

Those are the names as they sit on your `PATH`. From an unpacked tarball or
a build tree, reach for them where they are — `./opscope`, or
`./target/release/opscope`.

They are built to sit side by side and fill a wall, but nothing assumes a
multiplexer — each is an ordinary terminal program. Tile them however you
like.

## Building the wall

Fourteen widgets tile into whatever space you have. A layout that works on a
wide screen:

```
┌────────────────────┬──────────────────┬────────────┐
│ deployments        │ latency          │ clocks     │
├────────────────────┼──────────────────┴────────────┤
│ herdr-panes        │ tailnet                       │
└────────────────────┴───────────────────────────────┘
```

They degrade rather than break as panes get narrower: columns drop out in
priority order, footers wrap instead of truncating, and graphs rescale. A
widget in a 30-column strip still says something useful.

## Configuration

Every widget reads optional settings from the first readable of
`$OPSCOPE_CONFIG`, `$XDG_CONFIG_HOME/opscope/config.json`
(`~/.config/opscope/config.json` where that is unset), `config.json` in
the working directory, and `config.json` beside the binary. Copy
`config.example.json` to start.

This keeps hostnames, ping targets, city lists and tokens out of the source
tree: the repo ships generic defaults, and `config.json` is git-ignored along
with `.env` files and anything else likely to hold a secret.

**Three widgets need a token:** `deployments` wants a Vercel token from
Account Settings → Tokens, `github` a *classic* GitHub PAT with `repo` and
`read:org` (fine-grained tokens reach only one org each), and `linear` a
personal API key from Settings → Security & access. `pr` reuses the GitHub
token rather than asking for its own. Every other widget runs with no
configuration at all.

## Requirements

- **Nothing to install to run them** — the binaries carry what they link
  against, SQLite included. A Rust toolchain only if you build rather than
  download
- Linux x86-64 or macOS, Apple Silicon or Intel. A Linux arm64 build is not
  produced yet; that machine builds from source
- A terminal with 24-bit colour
- Per-widget, the external *tools* the table above names: `curl`, `ss`,
  `ping`, `tailscale`, `herdr`. Each widget needs only its own; one that
  cannot work without its tool says so rather than drawing an empty pane; and
  **none needs root**

## Documentation

Every widget has a page of its own — what it shows, where each number comes
from, every key it answers to, and the settings it reads. They are linked
from the table above, and listed together in [`docs/`](docs/README.md).

Four pages are about the repository rather than a widget:

| | |
|---|---|
| [Design conventions](docs/design.md) | the rules every widget holds to, and why each was paid for |
| [Internals](docs/internals.md) | `opscope-core`, the chart helpers, and what `cargo test` checks that a compiler cannot |
| [Port decisions](docs/port-decisions.md) | what the Rust port changed from the Python and why — the answer to most questions beginning *why does this key do that* |
| [Building Herdr panels](docs/building-herdr-panels.md) | resize semantics, focus, and the layout mistakes worth skipping |
| [Releasing](docs/releasing.md) | how a version is decided, what merging the release PR sets off, and what to do when it goes wrong |

## Bundled skill

[`skills/herdr/`](skills/herdr/) carries the Herdr control skill, so an agent
working in this repo can drive panes, tabs and workspaces without going looking
for it — copy it to `~/.claude/skills/herdr/` to install. It is Herdr's own
file, not covered by this repository's licence, and `herdr --skill` regenerates
it after an upgrade.

## What is being worked on

Planned widgets, open questions and the state of the Rust port are tracked
in Linear: <https://linear.app/stealth-company/project/opscope-e829b47d84b8/issues>. The link needs access to the workspace; the issues are the
canonical list either way, so a feature that looks missing may already be
filed there with a reason.

## Contributing

`cargo test` from the root is the gate: each widget's own tests, plus
[`widgets/tests/check.rs`](widgets/tests/check.rs), which reads the sources
and fails on the things a compiler cannot see — a poller that dies without
saying why, a key hinted but unanswered, a setting read but undocumented, a
colour below WCAG AA on a selected row. [Internals](docs/internals.md)
explains what each check is defending.

## License

[GNU AGPL-3.0](LICENSE). You may use, modify and share these widgets freely; if
you distribute a modified version — or run one as a network service — you must
make your source available under the same license.
