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
| the credential config names | `google::credential(&credentials, &credentials_file)` → `Result<Option<Credential>, String>` |
| nothing configured | `google::missing_credential(SECTION)` → the one row to show |
| a bearer token | `cred.bearer(&[google::SCOPE_…])` → `Result<Token, String>` |
| whom to grant access to | `cred.client_email()` → `Option<String>` (`None` when the key file cannot be read) |
| what to warn about | `cred.warning()` → a key file others can read |
| why a request was refused | `google::parse_refusal(status, &body).wording(&grant)` |

`credential` takes the JSON key in `credentials`, then the path in
`credentials_file`. A non-empty JSON string wins, and a string that is not
a service-account key is the error returned rather than a fall-through to
the file. A pasted access token lasts an hour and Google will not renew
it, so the helper does not read one.

`bearer` mints from the key file on first use and serves the cached token
until five minutes before it expires, per process and per (account,
scopes), so calling it before every request costs nothing. Its error is
already a sentence - an unreadable key file names the path, a refused
sign-in carries Google's `invalid_grant` description and a clock hint when
it is about `iat`/`exp` - so show it as it comes.

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

Declare `credentials` and `credentials_file` in the widget's `settings.json`.
`credentials` is the JSON key and is marked secret, so the screen masks it
and never copies a stored key back into the editor. `credentials_file` is
only a path.

```json
{
  "_schema": {
    "credentials": { "secret": true }
  },
  "_credentials_comment": "The service-account JSON key itself. Empty uses credentials_file. When both are set, this wins.",
  "credentials": "",
  "_credentials_file_comment": "Path to the service-account JSON key (~/ is expanded), used when credentials is empty. Keep it chmod 600 and outside any repository.",
  "credentials_file": ""
}
```

Read both in the widget with `cfg_str`, literally - the repository checks
read the widget's own source to find which keys it uses and whether their
defaults match `settings.json`, so the reads cannot move into core:

```rust
let credentials = tc::cfg_str(&cfg, "credentials", "");
let credentials_file = tc::cfg_str(&cfg, "credentials_file", "");
```

Regenerate `config.example.json` afterwards
(`UPDATE_CONFIG_EXAMPLE=1 cargo test`).

## Wording

Do not write your own line for "nothing configured". `missing_credential`
gives every Google widget the same one, naming the section's
`credentials` and `credentials_file` and ending in ``press `,` to set it here``:

```text
no credential: set google_analytics.credentials to the service-account
JSON, or google_analytics.credentials_file to the key's path
(docs/google-setup.md) — press `,` to set it here
```

Raise `cred.warning()` as a notice whenever it says something, with a
stable id, through `tc::notices::when` - not as a row beside the data. It is
`<path> is readable by others; chmod 600 it` - the key signs in as the
account indefinitely, so it is held to the same rule as `config.json`, whose
readable-by-others warning is a notice too: it explains no number on screen.

## A minimal widget

```rust
use opscope_core as tc;
use opscope_core::google;

const SECTION: &str = "google_example";

/// Sign in, or the one row that says why not.
fn bearer(cfg: &serde_json::Value) -> Result<(google::Credential, String), String> {
    let Some(cred) = google::credential(
        &tc::cfg_str(cfg, "credentials", ""),
        &tc::cfg_str(cfg, "credentials_file", ""),
    )?
    else {
        return Err(google::missing_credential(SECTION));
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
        None => "add the service account as Viewer on the property".into(),
    };
    Err(google::parse_refusal(status, &text).wording(&grant))
}
```

The two widgets stacked on this helper are fuller worked examples once
they land (OPS-186 and OPS-187): `google-analytics` keeps it in `api.rs`
(one `bearer` call per request), and `google-search-console` in `main.rs`'s
`signed_in()`.

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
  Use a distinct `client_email` per test, since the token cache is
  process-wide.
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
