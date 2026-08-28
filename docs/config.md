# `config`

[← all docs](README.md)

Every setting the widgets actually read, what it is set to now, and a way to
change it — instead of hand-editing a git-ignored JSON file and finding out
you were wrong by looking at an empty pane.

```
╺━ CONFIG ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 ~/.config/opscope/config.json
 73 keys · 12 unset · a running widget will not pick a change up until it restarts

▸ clocks.show_hints              true              true           set
  clocks.work_end_hour           18                18             set
  github.token                   ••••••••          ""             set
  github_actions.max_repos       16                16             set
  github_prs.sources             object · 3        object · 3     set
  agent_usage.grok_ping          —                 false          unset

 ── CLOCKS.SHOW_HINTS ──
 Whether the pomodoro key hints sit under the panel. Visible by default.
 restart clocks for a change to take effect

 ↑↓ select  ↵ edit  [s]how tokens  [r]eload  [c]opy  [q]uit
```

## The field list is the example

The rows are generated from `config.example.json` at build time. That file
is the schema `check.rs` already keeps honest — every key a widget reads is
in it, and every key in it is read by some widget — so a key added there
appears here with no second edit. The `_comment` strings become the per-field
help.

Nothing that is not in the example can be invented here. An undiscoverable
setting is not a setting.

## The file on the first line

`load_config` returns from the first path that parses, even when that file
has no such section. The path this pane shows is that file, so a write
cannot silently land somewhere nobody will read.

Search order is `$OPSCOPE_CONFIG`, `$TERMINAL_TOYS_CONFIG`,
`$XDG_CONFIG_HOME/opscope/config.json`, the legacy `terminal-toys/`
directory, `./config.json`, then beside the binary. A higher-priority file
that exists but does not parse is named rather than skipped in silence. If
nothing yet parses, the highest-priority location is shown and a save
creates it `0600`.

## Writes

A value is checked against the example's JSON type before anything is
written — a string where a number belongs is refused, and so is anything
that would leave the file unreadable. The write itself is a temp file in
the same directory, `0600` from creation, then a rename, so an interrupted
save cannot leave a truncated config that every other widget then fails to
parse.

Key order and `_comment` placement survive. The workspace `serde_json` has
no `preserve_order`, so a parse-and-dump would sort every object and hoist
`_comment` above the section it documents; this pane edits the raw text
instead and leaves every other byte alone.

A running widget will not pick the change up until it restarts. Every widget
calls `load_config` once, in `main()`. The selected row names the widget
that has to be restarted; this pane does not reach into other panes.

## Leftover section names

`agent-usage`, `github-prs` and `github-actions` still read a leftover
section (`usage`, `pr`, `gha`) when the new name is absent. The rows here
keep the example's names. A write goes to whichever object those widgets
would actually read — creating `github_prs` beside a leftover `pr` would
make the widget ignore the values it has been using. The pane names the
old section when that is what is happening.

## Tokens

Any key called `token`, or ending `_token`, is redacted until `s` reveals
it — `github.token`, `linear.token`, `deployments.token`, and
`github_actions.token` when that override is set. `c` copies a value that
is not a token, and refuses rather than putting a secret on the clipboard.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | move the list |
| `PgUp` `PgDn` `Home` `End` | jump the list; in the editor, `Home` / `End` move the caret |
| `↵` | open the selected field to edit, or toggle a boolean and write |
| `s` | show or hide tokens |
| `r` | reload the file from disk |
| `c` | copy the selected value, unless it is a token |
| `esc` | cancel an edit without writing |
| `q` | quit |

While editing, type a JSON value matching the example's shape. A string may
be typed without quotes. `↵` writes; `esc` cancels.

## Configuration

This widget has no section of its own. It reads and writes the resolved
config file, and the fields it shows are the ones the other widgets already
read.
