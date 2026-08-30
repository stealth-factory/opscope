# Configure `herdr-panes`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

The local `herdr` command's pane and process state.

## Settings owned here

The owned section is `herdr_panes`.

Declared fields: `refresh`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `herdr` exists and can list panes. No hostname, token, or workspace name belongs in tracked files.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Only refresh cadence is configurable; keep the default unless the user has a concrete responsiveness need.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
