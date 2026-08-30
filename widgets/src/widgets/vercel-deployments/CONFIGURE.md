# Configure `vercel-deployments`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

The Vercel API reached with `curl` and a user-created account token.

## Settings owned here

The owned section is `vercel_deployments`. A config written before the rename says `deployments`, and that is still read when the new one is absent.

Declared fields: `token`, `token_env`, `refresh`, `limit`, `teams`, `projects`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Check that `curl` exists and whether the configured token or named environment variable is present without printing its value.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask which teams/projects should be included. Never copy a token into chat, logs, command arguments, or source files.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
