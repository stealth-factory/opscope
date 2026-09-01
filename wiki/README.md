# opscope wiki

[← opscope](../README.md)

App-level knowledge: how opscope is built, how to extend it, and the reference
tables that outlive any one widget.

**What belongs here.** A page about *a* widget — what it shows, its keys, its
config — lives in that widget's own folder as its `README.md`. A page that
would have to be copied into fourteen of those to be useful belongs here.

| | |
|---|---|
| [Making a widget](making-a-widget.md) | the ordered fresh-checkout path from an empty folder to tests, launcher smoke test and release handoff, plus the contracts those steps defend |
| [Model prices](model-prices.md) | published API list prices per million tokens, with sources and as-of dates — what `agent-usage` multiplies by |

## Elsewhere

Not everything has moved here yet:

- [`docs/design.md`](../docs/design.md) — the visual language.
- [`docs/internals.md`](../docs/internals.md) — how `opscope-core` fits together.
- [`docs/port-decisions.md`](../docs/port-decisions.md) — what the Rust port
  changed from the Python, and why.
- [`docs/building-herdr-panels.md`](../docs/building-herdr-panels.md) — driving
  these from Herdr: resize, focus, layout.
- [`docs/releasing.md`](../docs/releasing.md) — the release pipeline.
- [`CLAUDE.md`](../CLAUDE.md) / [`AGENTS.md`](../AGENTS.md) — the same
  conventions, condensed for agents.

All five are cross-cutting and are candidates to move here.
