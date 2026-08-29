# Configure `ports`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Linux `/proc`, optional `ss` counters, and `tailscale serve status` for exposure.

## Settings owned here

The owned section is `ports`.

Declared fields: `system_ports`, `refresh`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Inspect listening ports only when useful. Treat addresses and project paths as local data and never put them in tracked examples.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask which ports are system services to hide and how often to refresh. Publishing actions remain interactive and are not settings.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen, restart the widget when it says restart is required, and verify that the real source answers or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
