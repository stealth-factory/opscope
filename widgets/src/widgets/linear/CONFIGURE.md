# Configure `linear`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

The Linear GraphQL API reached with `curl` and a personal API key.

## Settings owned here

The owned section is `linear`.

Declared fields: `token`, `token_env`, `exclude_teams`, `window_days`, `refresh`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `curl` and the key source without displaying the key. Team names must come from the user's Linear workspace, not guesses.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask which noisy teams, if any, should be excluded and what reporting window is useful.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen, restart the widget when it says restart is required, and verify that the real source answers or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
