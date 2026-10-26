# Building a widget on Google APIs

[← documentation](README.md)

Every widget that reads a Google API signs in through
`opscope_core::google` (`core/src/google.rs`). It turns a service-account
key into hour-long access tokens, caches and re-mints them, and words
Google's refusals - and the absence of any credential - the same way in
every pane. A new Google widget should not sign in, cache a token or word a
refusal on its own.

This page is for whoever writes the widget. What a *user* has to set up on
Google's side - the Cloud project, the APIs, the key, the access grant - is
[Google setup](google-setup.md); add your API and your access grant there.
The rest of the path (owned folder, launcher, settings, tests, hand-off) is
[Making a widget](../wiki/making-a-widget.md), as for any widget.

## What to call

| need | call |
|---|---|
| the credential config names | `google::credential(&credentials_file, &token, &token_env)` → `Option<Credential>` |
| nothing configured | `google::missing_credential(SECTION, &token_env)` → the one row to show |
| a bearer token | `cred.bearer(&[google::SCOPE_…])` → `Result<Token, String>` |
| whom to grant access to | `cred.client_email()` → `Option<String>` (none for a pasted token) |
| what to warn about | `cred.warning()` → a key file others can read, or a pasted token's one-hour life |
| why a request was refused | `google::parse_refusal(status, &body).wording(&grant)` |

`credential` takes the three settings in this order: the key file at
`credentials_file`, then `token` pasted into config, then the variable
`token_env` names (`GOOGLE_OAUTH_ACCESS_TOKEN` when blank). The key file
wins because it is the one that keeps working; a pasted token is for an
hour of testing.

`bearer` mints from the key file on first use and serves the cached token
until five minutes before it expires, per process and per (account,
scopes), so calling it before every request costs nothing. A pasted token
is returned as it is. Its error is already a sentence - an unreadable key
file names the path, a refused sign-in carries Google's `invalid_grant`
description and a clock hint when it is about `iat`/`exp` - so show it as
it comes.

`wording(grant)` is the shared line for a refused request: API not
enabled (naming the service), 403 with *your* remedy, 401, 429 with the
quota bucket and when it refills, or anything else with Google's message.
`grant` is the one part only the widget knows - which console, which role:
`add <email> as Viewer under Property access management` for Analytics,
`as a Restricted user` for Search Console. Build it from `client_email()`
so it names the address to add.

## Scopes

Ask for the narrowest read-only scope your API accepts, and only that one;
each widget gets a token of its own, so one key never gives a pane more
than it reads.

| constant | scope | used by |
|---|---|---|
| `google::SCOPE_ANALYTICS_READONLY` | `https://www.googleapis.com/auth/analytics.readonly` | `google-analytics` |
| `google::SCOPE_WEBMASTERS_READONLY` | `https://www.googleapis.com/auth/webmasters.readonly` | `google-search-console` |

A new API's scope goes into `core/src/google.rs` beside these, with a doc
comment saying which widget asks for it, not as a string in the widget.

## Settings to declare

Declare the three sign-in fields in the widget's `settings.json`, with
`token` marked secret. `token_env` ends in `_env`, so it stays visible; it
holds a variable's name, not a value.

```json
{
  "_schema": {
    "token": { "secret": true }
  },
  "_credentials_file_comment": "Path to the service-account JSON key (~/ is expanded), the same file every Google widget uses. Keep it chmod 600 and outside any repository. Wins over token and token_env.",
  "credentials_file": "",
  "_token_comment": "A pre-minted access token, for an hour of testing; Google tokens last one hour and this one is not renewed. Leave empty to read token_env.",
  "token": "",
  "_token_env_comment": "Environment variable holding a pre-minted access token, read when token is empty.",
  "token_env": "GOOGLE_OAUTH_ACCESS_TOKEN"
}
```

Read them in the widget with `cfg_str`, literally - the repository checks
read the widget's own source to find which keys it uses and whether their
defaults match `settings.json`, so the reads cannot move into core:

```rust
const TOKEN_ENV: &str = "GOOGLE_OAUTH_ACCESS_TOKEN";

let credentials_file = tc::cfg_str(&cfg, "credentials_file", "");
let token = tc::cfg_str(&cfg, "token", "");
let token_env = tc::cfg_str(&cfg, "token_env", TOKEN_ENV);
```

`TOKEN_ENV` is spelled out as a literal because
`a_declared_token_env_matches_the_code` reads it from `main.rs`; keep a test
holding it to `google::DEFAULT_TOKEN_ENV` so the two cannot drift.
Regenerate `config.example.json` afterwards
(`UPDATE_CONFIG_EXAMPLE=1 cargo test`).

## Wording

Do not write your own line for "nothing configured". `missing_credential`
gives every Google widget the same one, naming the section's
`credentials_file` and `token` and the variable actually read, and ending
in ``press `,` to set it here``:

```text
no credential: set google_analytics.credentials_file to a service-account
key file (docs/google-setup.md), or paste a token in google_analytics.token
or $GOOGLE_OAUTH_ACCESS_TOKEN — press `,` to set it here
```

Show `cred.warning()` beside the data whenever it says something. For a
key file it is `<path> is readable by others; chmod 600 it` - the key signs
in as the account indefinitely, so it is held to the same rule as
`config.json`. For a pasted token it is
`token from $GOOGLE_OAUTH_ACCESS_TOKEN · Google tokens last 1h`, so an
expired token is read as a refused token, not as a quiet site.
`tc::config_token_warning()` still applies on top, since `token` can sit
in `config.json`.

## A minimal widget

```rust
use opscope_core as tc;
use opscope_core::google;

const SECTION: &str = "google_example";
const TOKEN_ENV: &str = "GOOGLE_OAUTH_ACCESS_TOKEN";

/// Sign in, or the one row that says why not.
fn bearer(cfg: &serde_json::Value) -> Result<(google::Credential, String), String> {
    let token_env = tc::cfg_str(cfg, "token_env", TOKEN_ENV);
    let Some(cred) = google::credential(
        &tc::cfg_str(cfg, "credentials_file", ""),
        &tc::cfg_str(cfg, "token", ""),
        &token_env,
    ) else {
        return Err(google::missing_credential(SECTION, &token_env));
    };
    let token = cred.bearer(&[google::SCOPE_ANALYTICS_READONLY])?;
    Ok((cred, token.value))
}

/// One request, refused in the shared wording.
fn report(cred: &google::Credential, bearer: &str, url: &str, body: &str) -> Result<String, String> {
    let auth = format!("Bearer {}", bearer);
    let (status, text, _) = tc::post_answer(
        url,
        &[("Authorization", auth.as_str()), ("Content-Type", "application/json")],
        body,
        20,
    )?;
    if (200..300).contains(&status) {
        return Ok(text);
    }
    let grant = match cred.client_email() {
        Some(email) => format!("add {} as Viewer on the property", email),
        None => "give the token's account Viewer on the property".into(),
    };
    Err(google::parse_refusal(status, &text).wording(&grant))
}
```

The two widgets stacked on this helper are fuller worked examples once
they land (OPS-186 and OPS-187): `google-analytics` keeps it in `api.rs`
(one `bearer` call per request, a 401 on a pasted token carrying the
one-hour note), and `google-search-console` in `main.rs`'s `signed_in()`.

## Testing

No key material is ever in the tree - not a real key, not a fixture that
looks like one. The repository is public, and a scanner cannot tell a
throwaway key from a live one.

- **Keys are generated at test time.** `core/src/google.rs`'s tests make a
  throwaway 2048-bit RSA key once per run (`rsa::RsaPrivateKey::new`) and
  verify signatures against its public half. Where a test only needs
  something PEM-shaped, the armour is assembled from pieces so it never
  appears as one string in the source.
- **The network is handed in.** `access_token_with(key, scopes, now,
  exchange)` takes the clock and the HTTP exchange as arguments, so the
  cache and re-mint timing are tested without a network or a wait.
  `credential_from` takes the environment the same way. Use a distinct
  `client_email` per test, since the token cache is process-wide.
- **Refusals are tested from Google's bodies.** Paste the error JSON
  Google returns (with the project number and any address replaced by
  `example` values) into a test and assert the line `wording` produces.
- **The widget tests its own parsing and drawing.** Response parsers are
  `parse_*` functions over `&str`, in `parse.rs`, as for every widget;
  test the no-credential row with `google::missing_credential` rather than
  a copy of its text, so a change to the wording changes every widget's
  test with it.

Use `example.com`, `sc-domain:example.com`, `example-project` and
`…@example-project.iam.gserviceaccount.com` for every site, property,
project and account in a test or a doc. Never a real one, and never a
real property id.
