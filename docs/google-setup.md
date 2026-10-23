# Google setup

[← documentation](README.md)

The Google widgets - `google-analytics` and `google-search-console` - sign in
the same way: with a **service-account key**. The key signs a short request
on this machine, Google trades it for an access token that lasts an hour,
and the widget mints the next one five minutes before that runs out. No
browser, no refresh token that lapses after a week, no `gcloud`. The code is
`core/src/google.rs`; this page is what has to exist on Google's side first.
Writing a new Google widget is [its own page](google-widgets.md).

One key serves both widgets. Each asks only for its own scope -
`analytics.readonly` and `webmasters.readonly` - so each holds a token that
can read one product and nothing else, and neither can write anything.

## 1. One Cloud project

In the [Google Cloud console](https://console.cloud.google.com/), create a
project for the key to live in (or reuse one you already keep for tooling).
The project does not host anything and costs nothing; it is where the APIs
are switched on and where their quota is counted.

## 2. Enable the three APIs

Under **APIs & Services → Library**, enable:

| API | service name | read by |
|---|---|---|
| Google Analytics Data API | `analyticsdata.googleapis.com` | `google-analytics` - realtime and processed reports |
| Google Analytics Admin API | `analyticsadmin.googleapis.com` | `google-analytics` - listing the properties the key can see |
| Google Search Console API | `searchconsole.googleapis.com` | `google-search-console` |

An API left off is reported by name: the widget shows
`API not enabled: turn on <service> in the key's Cloud project` beside
Google's own message, rather than an empty pane.

## 3. Create the service account and its key

1. **IAM & Admin → Service accounts → Create service account.** Name it for
   what it does (for example `opscope-reader`). It needs **no** project role:
   access is granted on each property below, not on the Cloud project.
2. Open it, **Keys → Add key → Create new key → JSON**. The browser downloads
   one JSON file. That file is the credential: anybody holding it can read
   whatever the account was granted, for as long as the key exists.
3. Move it somewhere outside every repository and make it yours alone:

   ```sh
   mkdir -p ~/.config/opscope
   mv ~/Downloads/<downloaded-key>.json ~/.config/opscope/google-key.json
   chmod 600 ~/.config/opscope/google-key.json
   ```

   A key file that group or others can read is flagged on screen
   (`… is readable by others; chmod 600 it`), the same rule `config.json`
   is held to.
4. Note the account's email (`client_email` in the file, also shown in the
   console). It is the address you grant access to next.

If your organisation's policy forbids key creation
(`iam.disableServiceAccountKeyCreation`), an administrator has to allow it
for this project; there is no keyless route that works on a wall display.

## 4. Grant it access, least privilege

**Google Analytics (GA4).** In GA, **Admin → Property access management →
+ → Add users**, enter the service account's email and give it **Viewer**.
Viewer reads reports and nothing else. Repeat for each property the widget
should show. Leaving it out is reported as
`403 no access: add <email> as Viewer under Property access management`.

**Search Console.** In Search Console, pick the property, **Settings →
Users and permissions → Add user**, enter the same email, and choose
**Restricted**. Restricted reads performance data and cannot change the
property, submit sitemaps or add users.

Access can take a few minutes to reach the APIs after it is granted.

## 5. Point the widget at the key

Each widget's section takes the path, and only the path - the key itself
never goes in `config.json`:

```json
"google_analytics": {
  "credentials_file": "~/.config/opscope/google-key.json"
}
```

Each widget's own `CONFIGURE.md` names its section and its other settings.

## For an hour of testing: a pasted token

Without a key, a token minted elsewhere works until it expires. It has to
carry the widget's scope - `analytics.readonly` for `google-analytics`,
`webmasters.readonly` for `google-search-console` - and a plain
`gcloud auth print-access-token` for your own account cannot ask for
either: for a user account gcloud only accepts a fixed list of scopes. The
scope has to be granted at login first, through Application Default
Credentials:

```sh
gcloud auth application-default login --scopes=\
https://www.googleapis.com/auth/cloud-platform,\
https://www.googleapis.com/auth/analytics.readonly,\
https://www.googleapis.com/auth/webmasters.readonly
export GOOGLE_OAUTH_ACCESS_TOKEN="$(gcloud auth application-default print-access-token)"
```

Google can refuse those scopes for gcloud's own sign-in client on some
accounts; if it does, the service-account key above is the way in. Someone
holding *Service Account Token Creator* on the service account can instead
mint a token as it, which carries exactly the access the account was given:

```sh
export GOOGLE_OAUTH_ACCESS_TOKEN="$(gcloud auth print-access-token \
  --impersonate-service-account=<service-account-email> \
  --scopes=https://www.googleapis.com/auth/analytics.readonly,\
https://www.googleapis.com/auth/webmasters.readonly)"
```

Both scopes are asked for there so one token serves either widget; drop
the one you do not need.

The widget reads the variable named by `token_env` (that one by default), or
`token` in its config section. It cannot renew it, so the screen says
`token from $GOOGLE_OAUTH_ACCESS_TOKEN · Google tokens last 1h`, and an
expired one is reported as a refused token rather than as a quiet site. A
`credentials_file` that is set wins over both.

## What Google's refusals look like on screen

| Google says | the widget shows |
|---|---|
| sign-in `invalid_grant` | `Google refused the sign-in: invalid_grant - <Google's description>`, plus a hint to check the clock when the description is about `iat` / `exp` |
| 403, API disabled | `API not enabled: turn on <service> …` and Google's message |
| 403 on the resource | `403 no access: <what to grant>` and Google's message |
| 401 | `401 the access token was refused (expired or revoked)` |
| 429 | `429 quota: <bucket> exhausted (<when it refills>)` and Google's message |

## Why not the alternatives

- **OAuth user sign-in with a refresh token.** The device flow does not
  allow the `analytics.readonly` or `webmasters.readonly` scopes, a refresh
  token expires after seven days while the consent screen is in "Testing",
  and it puts a client secret in config.
- **`gcloud auth print-access-token`.** Its own sign-in cannot ask for
  these scopes without a scoped `application-default` login first, its
  tokens still last an hour, and it is a 300 MB tool on every machine that
  runs a pane. Fine for an hour of testing, as above; not for a wall.
- **A JWT crate.** `jsonwebtoken` brings `aws-lc-rs`, which needs a C
  compiler, and `ring` needs `cc`. The JWT is forty lines over `rsa` and
  `sha2`, both pure Rust, so `ldd` on a release binary still shows only libc,
  libm and libgcc.

`rsa` carries advisory RUSTSEC-2023-0071, a timing side channel (Marvin)
that lets somebody who can time many private-key operations recover the
key. It does not apply here: the key signs one JWT an hour on this machine,
and nothing exposes the timing of that to anybody.
