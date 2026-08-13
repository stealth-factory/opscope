# terminal-toys

Small terminal widgets with no dependencies — pure Python 3 standard library,
24-bit colour, and a full redraw each frame so everything reflows when you
resize the pane.

Each widget is a single self-contained script that imports shared drawing
helpers from `common.py`. Run any of them directly; pass `-h` for its own
documentation.

## Widgets

| Widget | What it answers | Needs |
|---|---|---|
| [`latency.py`](docs/latency.md) | Is the network to these hosts healthy — and *why* does it feel bad? | `ping` |
| [`deployments.py`](docs/deployments.md) | How are Vercel deployments going over time, and where is that preview URL? | a Vercel token |
| [`tailnet.py`](docs/tailnet.md) | Who is on my tailnet, and am I reaching them directly or through a relay? | `tailscale` |
| [`herdr-panes.py`](docs/herdr-panes.md) | What is running across every workspace, and which agent needs me? | `herdr` |
| [`clocks.py`](docs/clocks.md) | What time is it here and elsewhere, and how much of it is left? | — |
| `matrix.py` | Nothing whatsoever. Digital rain, with truecolor fade trails. | — |

```sh
./latency.py            # each runs standalone
./clocks.py -h          # every widget documents itself
```

They are built to sit side by side in a multiplexer, but nothing assumes one —
each is an ordinary terminal program.

## Configuration

Every widget reads optional settings from the first of
`$TERMINAL_TOYS_CONFIG`, `~/.config/terminal-toys/config.json`, or
`config.json` beside the scripts. Copy `config.example.json` to start.

This keeps hostnames, ping targets, city lists and tokens out of the source
tree: the repo ships generic defaults, and `config.json` is git-ignored along
with `.env` files and anything else likely to hold a secret.

**Only `deployments.py` requires configuration** — a Vercel token, created at
Account Settings → Tokens. Every other widget runs with none.

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
24-bit colour, progress bars, a green→amber→red `heat()` ramp, `seg()` for
clipping coloured text to a cell budget, `pack_hints()` for wrapping footers,
non-blocking `Keyboard` input with arrow-key decoding, and `clipboard()` over
OSC 52.

## License

[GNU AGPL-3.0](LICENSE). You may use, modify and share these widgets freely; if
you distribute a modified version — or run one as a network service — you must
make your source available under the same license.
