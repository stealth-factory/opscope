# Configure `netwatch`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Linux `/proc` socket ownership plus TCP counters from `ss -tine`.

## Settings owned here

The owned section is `netwatch`.

Declared fields: `interval`, `limit`, `sort`, `external`, `mine`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `ss` and `/proc` are available. Process names are local observations, not configuration values.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask about polling cadence, row limit, sort order, and whether external/local traffic should be included.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
