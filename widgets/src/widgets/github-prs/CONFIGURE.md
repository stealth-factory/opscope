# Configure `github-prs`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

GitHub search and pull-request APIs using the shared GitHub credential.

## Settings owned here

The owned section is `github_prs`. A legacy `pr` section is used only when `github_prs` is absent.

Declared fields: `sources`, `limit`, `refresh`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Inspect the shared `github` token source without printing it. Read existing search strings before proposing a replacement.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask what follow-up queues are wanted. Preserve `@me` and `@mine` semantics unless the user explicitly changes the ownership model.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen, restart the widget when it says restart is required, and verify that the real source answers or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
