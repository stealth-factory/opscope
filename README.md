# terminal-toys

**Sci-fi hacker terminal toys from the movies — except these tell you the
truth.**

Fill your screen with glowing panels, scrolling graphs and blinking status
readouts. The difference is that every number on them is real: actual network
latency, actual deployments, actual machines on your tailnet, actual agents
waiting on you.

[![terminal-toys — click to watch the 26-second demo](docs/demo-poster.jpg)](docs/demo.mp4)

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

Some of the theatre is still here, unapologetically. `matrix.py` computes
nothing at all — it just looks good, and it knows it.

## The widgets

| Widget | What it does | Needs | Docs |
|---|---|---|---|
| **`latency.py`** | Continuous latency to a list of hosts: median, jitter, loss and a log-scale graph, so a slow link and an *unsteady* one look different. | `ping` | [read →](docs/latency.md) |
| **`deployments.py`** | Vercel deployments over time — activity per hour, build-time drift, and a copy sheet for the dashboard, preview and PR URLs. | a Vercel token | [read →](docs/deployments.md) |
| **`tailnet.py`** | Tailscale peers, and whether each is reached directly or through a relay. Live throughput, full machine info, copyable addresses. | `tailscale` | [read →](docs/tailnet.md) |
| **`herdr-panes.py`** | Every agent and process across all workspaces, ordered by which one needs a human. Enter jumps you there. | `herdr` | [read →](docs/herdr-panes.md) |
| **`github.py`** | Pull requests across every org: merge rate, opened-vs-merged per day, review backlog and the contribution calendar. | a GitHub token | [read →](docs/github.md) |
| **`pr.py`** | The pull requests you have to follow up on: checks, reviews, mergeability, and a stack map with the order a stack has to merge in. | a GitHub token | [read →](docs/pr.md) |
| **`linear.py`** | Linear across every team: what is outstanding, the running cycles and their scope creep, and issues created against completed. | a Linear API key | [read →](docs/linear.md) |
| **`usage.py`** | How much each coding agent on the machine has been used — tokens, sessions, AI-written code — one tab per agent, from local files only. | — | [read →](docs/usage.md) |
| **`clocks.py`** | Server clock, countdowns to the next hour / end of office hours / end of day, a pomodoro, and a world clock. | — | [read →](docs/clocks.md) |
| **`matrix.py`** | Nothing whatsoever. Digital rain, with truecolor fade trails. | — | — |

Each is a single self-contained script with no dependencies — pure Python 3
standard library, 24-bit colour, and a full redraw each frame so everything
reflows when you resize the pane.

```sh
./latency.py            # each runs standalone
./clocks.py -h          # every widget documents itself
```

They are built to sit side by side and fill a wall, but nothing assumes a
multiplexer — each is an ordinary terminal program. Tile them however you like.

## Building the wall

Ten widgets tile into whatever space you have. A layout that works on a wide
screen:

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

Every widget reads optional settings from the first of
`$TERMINAL_TOYS_CONFIG`, `~/.config/terminal-toys/config.json`, or
`config.json` beside the scripts. Copy `config.example.json` to start.

This keeps hostnames, ping targets, city lists and tokens out of the source
tree: the repo ships generic defaults, and `config.json` is git-ignored along
with `.env` files and anything else likely to hold a secret.

**Three widgets need a token:** `deployments.py` wants a Vercel token from
Account Settings → Tokens, `github.py` a *classic* GitHub PAT with `repo` and
`read:org` (fine-grained tokens reach only one org each), and `linear.py` a
personal API key from Settings → Security & access. `pr.py` reuses the GitHub
token rather than asking for its own. Every other widget runs with no
configuration at all.

## Requirements

- Python **3.9+** (`clocks.py` uses `zoneinfo`); developed on 3.12
- A terminal with 24-bit colour
- Per-widget: `ping`, `tailscale` or `herdr` as listed above. Each needs only
  its own, and **none needs root**

## Design

A few conventions hold across all of them, and the reasoning is worth knowing
before changing one:

- **Spend extra width on more content, not padding.** Widgets add columns as a
  pane grows and drop them as it shrinks, rather than truncating.
- **Never truncate a key hint.** Footers wrap across as many lines as they need
  and never split a hint, because `[±]25` teaches a key that does not exist.
- **Measure contrast, do not eyeball it.** Every colour that draws text clears
  WCAG AA against both the terminal background *and* the selected-row tint,
  with the measured ratios recorded beside the definitions.
- **Say what a number means when it is not obvious.** Counters that reset with
  a daemon, durations that predate the process, aggregates that hide their
  outliers — each is labelled rather than left to mislead.
- **Never show a stale figure under a fresh label.** Change a setting and the
  numbers it governs shimmer until real ones land, rather than sitting there
  looking current. The same rule kills silent truncation: a chart that cannot
  fit its window says `54d of 90d`, and a token missing a scope is named rather
  than left to quietly undercount.
- **Optional enhancements, never requirements.** The clipboard goes through
  OSC 52 so it survives SSH; Herdr toasts and `sudo`-gated data are added where
  available and skipped silently where not.

## Bundled skill

[`skills/herdr/`](skills/herdr/) carries the Herdr control skill, so an agent
working in this repo can drive panes, tabs and workspaces without going looking
for it — copy it to `~/.claude/skills/herdr/` to install. It is Herdr's own
file, not covered by this repository's licence, and `herdr --skill` regenerates
it after an upgrade.

## Building your own

[`docs/building-herdr-panels.md`](docs/building-herdr-panels.md) collects what
was learned building these against Herdr: resize semantics, focus, detecting
what a pane is running, notification gating, and the layout mistakes worth
skipping.

`common.py` holds the shared pieces — terminal sizing, a full-frame `draw()`,
24-bit colour, a green→amber→red `heat()` ramp, `seg()` for clipping coloured
text to a cell budget, `pack_hints()` for wrapping footers, non-blocking
`Keyboard` input with arrow-key decoding, and `clipboard()` over OSC 52.

The chart helpers are worth knowing before drawing anything new: `vbars()` and
its mirror `vbars_down()` (pair them on a shared scale for a diverging chart),
`braille_plot()` for continuous lines at 2×4 sub-pixels a cell, `stacked_bar()`
for proportions, `meter()` for a gauge, and `skeleton()` for the shimmer that
stands in for a figure still being fetched.

## License

[GNU AGPL-3.0](LICENSE). You may use, modify and share these widgets freely; if
you distribute a modified version — or run one as a network service — you must
make your source available under the same license.
