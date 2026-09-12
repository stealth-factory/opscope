# Configure `luvus-panes`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

A running [Luvus](https://luvus.dev) session, read over Universal Harness Protocol 1.0 through the local `luvus` command. Eight read-only calls: `uhp snapshot`, `agent list --json`, `task list --json`, `lease list --json`, `task next`, `agent sessions`, `git status`, and `worktree list`. Nothing else is asked for, and nothing is written except the pane focus `↵` performs.

## Settings owned here

The owned section is `luvus_panes`.

Declared fields: `session`, `refresh`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `luvus` exists and answers — `luvus ping` is the cheapest check, and `luvus session list` names the sessions that exist on this machine. No hostname, socket path, token, or workspace name belongs in a tracked file.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. `session` is only worth changing on a machine running more than one Luvus server. The name must be one `luvus session list` reports; a name with no server behind it draws the "no luvus server" screen rather than an empty board.
4. `refresh` is the cadence of those four calls. Keep the default unless the user has a concrete responsiveness need.
5. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
6. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real session answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate session names, workspace names, agent names, branches, task ids, reserved paths, pane ids, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
