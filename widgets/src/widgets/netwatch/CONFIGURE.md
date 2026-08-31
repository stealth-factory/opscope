# Configure `netwatch`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real sources

- Linux: `/proc` socket ownership plus TCP counters from `ss -tine`.
- macOS: all-protocol process counters from `nettop`, interface counters from
  `netstat -ib`, and process facts from `ps`/`lsof`.

## Settings owned here

The owned section is `netwatch`.

Declared fields: `interval`, `limit`, `sort`, `external`, `mine`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm the pane names the platform source, or gives the precise missing
   source. Process names are local observations, not configuration values.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask about polling cadence, row limit, and sort order. Peer filtering is
   Linux-only because macOS `nettop` process rows carry no peer field.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
