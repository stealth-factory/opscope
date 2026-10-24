# Configure `google-analytics`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Google's Analytics Data API (`runRealtimeReport`, `batchRunReports`, `runReport`) and Analytics Admin API (account summaries, the property, its key events), reached with `curl` and a token minted from a Google service-account key. The realtime baseline is the one figure not read from Google: it comes from the widget's own sample file under `$XDG_STATE_HOME/opscope/google-analytics/`.

## Settings owned here

The owned section is `google_analytics`.

Declared fields: `credentials_file`, `token`, `token_env`, `properties`, `realtime_refresh`, `refresh`, `window_days`, `baseline_weeks`, `key_events`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## What has to exist on Google's side first

Follow `docs/google-setup.md` in this repository: one Cloud project, the Analytics Data API and Analytics Admin API enabled (the Search Console API too if `google-search-console` will share the key), a service account with a JSON key saved outside any repository and `chmod 600`, and the service account's email added as **Viewer** on each GA4 property under Property access management.

These steps are the user's to take in Google's consoles. Do not attempt them on the user's behalf and do not guess whether they are done: the widget names each missing step on screen (API not enabled, no access, `invalid_grant`).

## Safe configuration process

1. Check that `curl` exists.
2. Ask the user for the path to their service-account key. Check that the file exists and its mode is `600` without printing its contents - the key is a credential that signs in indefinitely. Never read, copy, paste or summarise the key's `private_key`.
3. Ask which GA4 properties to show. A property id is the number after `p` in GA's admin URLs (`…/a<account>p<property>/…`); only that number goes in `properties`. Leave `properties` empty to show every property the service account can see. Never invent an id.
4. Ask whether only some events should count as key events; by default every event GA flags as a key event is used.
5. Change only this widget's declared section, through the settings screen. Keep secrets and property ids out of chat logs, shell history, source files, commits and screenshots - this repository is public.
6. Leave the settings screen; the widget reloads itself. Then verify that the header shows a realtime reading and a quota line, or that a row names exactly what Google refused.

## Testing without a key

`token` (or the variable named by `token_env`, `GOOGLE_OAUTH_ACCESS_TOKEN` by default) takes a pre-minted access token with the `analytics.readonly` scope. It lasts one hour and cannot be renewed by the widget; the screen says where it came from and that it expires. `credentials_file` wins when both are set.

## Boundaries

Do not fabricate property ids, account emails, key event names, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
