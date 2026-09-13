# opscope as a Luvus module

[← opscope](../README.md)

Every widget as a pane inside a [Luvus](https://luvus.dev) session, plus the
launcher menu. Luvus runs a module's declared commands as subprocesses, and
`[[panes]]` opens a real pane running one — which is the shape these binaries
already have, so there is nothing here but the manifest and a script that
fetches them.

```sh
luvus module install stealth-factory/opscope/luvus
luvus module pane open opscope.widgets ports --placement split
```

`luvus module info opscope.widgets` lists every pane it declares.

## What it declares

One pane per widget, plus `menu` for the launcher — the front door that shows
every widget and a preview before it runs. Every pane goes through
`./bin/opscope <widget>` rather than at the widget binary directly: named a
widget, the launcher draws no menu, starts it, and exits with its status — so
the pane closes when you quit the widget, and the old names the launcher
resolves keep working. It waits as the parent while the widget runs, which is
one extra process per pane and the same shape `npx opscope <widget>` already
has. Closing the pane takes both.

Nothing else. No dock, no bar, no settings, no event hooks — this module
reads nothing about your session and writes nothing to it.

## Where the binaries come from

`[[build]]` runs `build.sh` once at install time. It reads the version out
of the manifest beside it, fetches that release's tarball for this host from
GitHub, checks it against the `.sha256` published beside it, and leaves the
binaries in `bin/`.

The version comes from the manifest and never from `/releases/latest`. Luvus
pins an installed module to the commit you reviewed, and a build that fetched
whatever release existed at install time would quietly untie those two.

Published: Linux x86_64, macOS arm64, macOS x86_64. Anything else stops with
a sentence naming what exists rather than installing a module that cannot
start.

## Configuration

The same `config.json` every other install reads — `~/.config/opscope/config.json`
by default, or wherever `OPSCOPE_CONFIG` points. Tokens and host lists set
once work in a pane, under `npx`, and from a shell, and this module declares
no settings of its own so there is only ever one place they live. `,` opens
the settings screen inside any widget.

## What it needs

`curl` or `wget`, `tar`, and `shasum` or `sha256sum` at install time. After
that the binaries carry everything they need; the widgets that read host
tools (`ss`, `ping`, `tailscale`, `luvus` itself) say so and stop rather than
drawing an empty pane. `./bin/opscope doctor` reports what this machine has.

Written against the manifest features Luvus documents at **0.8.3**, which is
what `min_luvus_version` says. Run against **0.13.4**, which is a different
claim and the one worth trusting.

## The manifest is generated

`luvus-module.toml` is written by `cargo test` from the widget folders on
disk, so adding a widget adds a pane and a rename cannot leave a pane
pointing at a binary that is gone. After changing the widget list or the
version:

```sh
UPDATE_LUVUS_MODULE=1 cargo test --test check \
  generated_luvus_module_matches_the_widgets -- --exact
```

`cargo test` fails when the committed file and the folders disagree. The
generator is in [`widgets/tests/check.rs`](../widgets/tests/check.rs).

## Trust

A module is ordinary code that runs as you. Luvus shows every command a
module declares before it installs one, and the whole of this module's is
above: one `/bin/sh build.sh`, and one `./bin/opscope` line for each of the
sixteen widgets and the launcher menu.
