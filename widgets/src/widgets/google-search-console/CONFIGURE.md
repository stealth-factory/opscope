# Configure `google-search-console`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Google's Search Console API, read with curl and the read-only `webmasters.readonly` scope. Sign-in is a service-account key handled by `core/src/google.rs`: the key signs a JWT on this machine and Google trades it for an hour-long access token. The Google side of the setup - one Cloud project, the Search Console API enabled, a service account with a key, and that account added as a **Restricted** user on each property - is written up in `docs/google-setup.md`.

## Settings owned here

The owned section is `google_search_console`.

Declared fields: `credentials_file`, `token`, `token_env`, `sites`, `refresh`, `window_days`, `inspect_top`, `search_type`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

`credentials_file` is the same key file `google_analytics.credentials_file` points at; one key serves both widgets, each asking for its own scope.

## Safe configuration process

1. Check that `curl` exists. Check whether `credentials_file` is set and whether the file exists and is mode 600, without printing its contents. Check whether the variable named by `token_env` is set, without printing its value.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask which properties should be shown, and in which form Search Console names each one: `sc-domain:<domain>` for a domain property, or the URL with its trailing slash for a URL-prefix property. Do not guess a property from a domain name; leaving `sites` empty reads every property the account can see.
4. Never copy the key file's contents, a token, or the service account's private key into chat, logs, command arguments, or source files. The key file stays outside every repository.
5. Change only this widget's declared section, through the settings screen. The widget reloads itself on the way out.
6. Verify that a real answer arrives, or that the pane names the reason it cannot: no credential, the API not enabled, the account not a user on the property, an unverified user, or a quota.

## Facts to ask rather than guess

- Which Google Cloud project and service account to use, and where its key file is kept.
- Which properties to show, and their exact Search Console names.
- Whether spending up to `inspect_top` URL Inspections per property per day (of Google's 2,000) is wanted; 0 turns inspection off.

## Boundaries

Do not fabricate property names, domains, service-account emails, key material, tokens, or successful readings. Search Console cannot be written to through this widget; it holds a read-only scope. If a value cannot be established from the local environment or the user's explicit instruction, ask.
