# opscope as a Luvus module

[← opscope](../README.md)

Every widget as a pane inside a [Luvus](https://luvus.dev) session, plus the
launcher menu. Luvus runs a module's declared commands as subprocesses, and
`[[panes]]` opens a real pane running one — which is the shape these binaries
already have, so there is nothing here but the manifest and a script that
fetches them.

```sh
luvus module install stealth-factory/opscope
```

## The way in is the launcher

**Right-click any pane and choose *opscope*.** That opens the launcher: all
sixteen widgets, each with its own one-line summary and a preview of what it
looks like running, and `↵` starts the one under the cursor in that same pane.
Browsing is the launcher's whole job and there is no reason to make anyone do
it from a command line.

That is the only thing this module puts in Luvus's interface, deliberately.
Luvus shows a module's **actions** in its right-click menus and its declared
**panes** nowhere at all — so a module with seventeen panes and no action
installs successfully and then appears nowhere, which is how this started.

**If you already know which widget you want, name it and skip the menu:**

```sh
luvus module pane open opscope.widgets ports
luvus module pane open opscope.widgets clocks --placement overlay
```

Every widget has a pane of its own, so anything on the launcher's list is
also a command. `--placement` takes `split`, `overlay` or `tab`; left out, the
manifest's own `split` stands. `luvus module info opscope.widgets` lists all
seventeen.

## What it declares

One pane per widget, plus `menu` for the launcher, and one action —
`open-menu` — which is the right-click row above. An action is a
fire-and-forget subprocess with no terminal of its own, so it cannot *be* a
widget: `luvus/open-menu.sh` asks luvus for a pane and lets the declaration
decide where it lands.

Every pane goes through `./luvus/bin/opscope <widget>` rather than at the
widget binary directly: named a widget, the launcher draws no menu, starts it,
and exits with its status — so
the pane closes when you quit the widget, and the old names the launcher
resolves keep working. It waits as the parent while the widget runs, which is
one extra process per pane and the same shape `npx opscope <widget>` already
has. Closing the pane takes both.

Nothing else. No dock, no bar, no settings, no event hooks — this module
reads nothing about your session and writes nothing to it. The one action it
declares opens a pane and takes no argument; it cannot reach anything you are
running.

## Where the binaries come from

`[[build]]` runs `luvus/build.sh` once at install time. It reads the version
from the manifest at the module root, fetches that release's tarball for this
host from GitHub, checks it against the `.sha256` published beside it, and
leaves the binaries in `luvus/bin/` — which is what the pane commands point
at. It also refuses a tarball that is missing any widget the manifest declares
a pane for, since a pane with no binary behind it is a menu entry that dies on
open.

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
drawing an empty pane. `./luvus/bin/opscope doctor` reports what this machine has.

Written against the manifest features Luvus documents at **0.8.3**, which is
what `min_luvus_version` says. Run against **0.13.4**, which is a different
claim and the one worth trusting.

## Upgrading, and going back to a working tree

There is no `luvus module update`. `install` on an id that is already
registered is an error — and it is raised *after* the clone, the download and
the checksum, so a cheap mistake looks expensive. Upgrading is two commands:

```sh
luvus module uninstall opscope.widgets     # unlink, and delete the checkout
luvus module install stealth-factory/opscope
```

`uninstall` deletes the managed checkout; `unlink` on its own only
deregisters and leaves your files alone, which is what you want for a module
you linked rather than installed:

```sh
luvus module link /path/to/opscope         # develop against a working tree
luvus module unlink opscope.widgets        # and stop
```

Both forms claim the same id, so only one can be registered at a time. A
`link` left over from development is the likeliest reason an install stops
with *module opscope.widgets is already registered*.

## The manifest is generated

`luvus-module.toml` — at the repo root, because a manifest in a subdirectory
can only be installed as `owner/repo/sub` and `owner/repo` is the line the
module index prints under every listing — is written by `cargo test` from the
widget folders on disk, so adding a widget adds a pane and a rename cannot leave a pane
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
above: one `/bin/sh luvus/build.sh` at install time, one `/bin/sh
luvus/open-menu.sh` behind the right-click row, and one `./luvus/bin/opscope`
line for each of the sixteen widgets and the launcher menu.
