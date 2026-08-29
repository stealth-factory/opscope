# Configure `github-actions`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

GitHub Actions APIs for the configured/discovered accounts and repositories.

## Settings owned here

The owned section is `github_actions`. A legacy `gha` section is used only when `github_actions` is absent.

Declared fields: `token`, `token_env`, `accounts`, `repos`, `window_hours`, `refresh`, `max_repos`, `pushed_days`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Inspect the shared `github` token source first; this widget's token fields are overrides. Never display either token.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask whether discovery or an explicit account/repository set is wanted, and explain the `max_repos`, pushed-age, and time-window limits before changing them.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen, restart the widget when it says restart is required, and verify that the real source answers or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
