//! Google sign-in for the widgets that read Google's APIs.
//!
//! A service-account key is turned into short-lived access tokens here, so
//! every Google widget signs in the same way and words Google's refusals
//! the same way.
//!
//! The route is the one that keeps a wall display lit: the key signs an
//! RS256 JWT locally, Google trades it for an access token that lasts an
//! hour, and the next one is minted five minutes before that runs out. No
//! browser, no refresh token that expires after seven days in "Testing",
//! no client secret, and no `gcloud`. Each widget asks for its own scope,
//! so one key gives each widget a least-privilege token of its own.
//!
//! The JWT is about forty lines rather than a crate: `jsonwebtoken` brings
//! `aws-lc-rs` (a C compiler) or the whole RustCrypto suite, and `ring`
//! wants `cc`. Signing is `rsa`'s PKCS#1 v1.5 over SHA-256, both pure Rust.
//!
//! The fallback is a pre-minted access token in config or an environment
//! variable, which a developer can paste from `gcloud` for an hour of
//! testing. The screen says where it came from and that it lasts an hour,
//! so an expired token is not mistaken for a quiet site.

use base64::Engine;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Mutex;

/// Where a key file sends its JWT when it does not say.
pub const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// The variable a pasted token is read from by default - the name
/// `gcloud auth print-access-token` users already reach for.
pub const DEFAULT_TOKEN_ENV: &str = "GOOGLE_OAUTH_ACCESS_TOKEN";

/// Read-only Google Analytics: the Data API's reports and the Admin API's
/// reads. What `google-analytics` asks for.
pub const SCOPE_ANALYTICS_READONLY: &str = "https://www.googleapis.com/auth/analytics.readonly";

/// Read-only Search Console: search analytics, URL Inspection, sites and
/// sitemaps, and nothing that changes a property. What
/// `google-search-console` asks for.
pub const SCOPE_WEBMASTERS_READONLY: &str = "https://www.googleapis.com/auth/webmasters.readonly";

/// How long Google lets an assertion, and the token it buys, live.
pub const TOKEN_LIFETIME: u64 = 3600;

/// How long before expiry a cached token is replaced. Five minutes, so a
/// request that starts with a token never arrives holding a dead one.
pub const REMINT_EARLY: u64 = 300;

/// The three fields of a service-account key that signing needs.
///
/// `Debug` is written by hand so the private key never reaches a log, a
/// panic message or a test failure.
#[derive(Clone, PartialEq)]
pub struct ServiceAccountKey {
    /// The service account's address: the JWT's issuer, and the address
    /// access is granted to on each property.
    pub client_email: String,
    /// The PKCS#8 PEM private key the JWT is signed with.
    pub private_key: String,
    /// Where the signed JWT is traded for a token; `DEFAULT_TOKEN_URI`
    /// when the file does not say.
    pub token_uri: String,
}

impl std::fmt::Debug for ServiceAccountKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccountKey")
            .field("client_email", &self.client_email)
            .field("private_key", &"<redacted>")
            .field("token_uri", &self.token_uri)
            .finish()
    }
}

/// Read the JSON key Google's console downloads for a service account.
///
/// A user credential (`authorized_user`) is refused by name rather than
/// failing later on a missing field, because it is the file people most
/// often have lying around from `gcloud auth application-default login`.
pub fn parse_service_account_key(text: &str) -> Result<ServiceAccountKey, String> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| format!("the key file is not JSON ({})", e))?;
    if !v.is_object() {
        return Err("the key file is not a JSON object".into());
    }
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("service_account");
    if kind != "service_account" {
        return Err(format!(
            "the key file is a {} credential, not a service-account key",
            kind
        ));
    }
    let field = |name: &str| -> Result<String, String> {
        match v.get(name).and_then(|f| f.as_str()) {
            Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
            _ => Err(format!("the key file has no {}", name)),
        }
    };
    let client_email = field("client_email")?;
    let private_key = field("private_key")?;
    let token_uri = v
        .get("token_uri")
        .and_then(|t| t.as_str())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(DEFAULT_TOKEN_URI)
        .to_string();
    if !token_uri.starts_with("https://") {
        return Err(format!("the key file's token_uri is not https: {}", token_uri));
    }
    Ok(ServiceAccountKey { client_email, private_key, token_uri })
}

/// `~/` expanded, since that is how people write a path in a config file.
pub fn expand_home(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) if !home.is_empty() => format!("{}/{}", home.trim_end_matches('/'), rest),
            _ => path.to_string(),
        },
        None => path.to_string(),
    }
}

/// Read and parse the key file at `path`.
pub fn read_key_file(path: &str) -> Result<ServiceAccountKey, String> {
    let full = expand_home(path);
    let text = std::fs::read_to_string(&full)
        .map_err(|e| format!("cannot read the key file {}: {}", path, e))?;
    parse_service_account_key(&text).map_err(|e| format!("{}: {}", path, e))
}

/// Warn when a key file is readable by anyone but its owner.
///
/// The same rule as `config_token_warning`: a key that signs in as the
/// service account indefinitely is worth more than any token in config.
pub fn key_file_warning(path: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(expand_home(path)).ok()?.permissions().mode();
    key_mode_warning(path, mode)
}

/// The decision `key_file_warning` makes, apart from the file system.
pub fn key_mode_warning(path: &str, mode: u32) -> Option<String> {
    if mode & 0o077 != 0 {
        Some(format!("{} is readable by others; chmod 600 it", path))
    } else {
        None
    }
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A signed JWT asking `key.token_uri` for a token with `scopes`.
///
/// `iat` is `now` and `exp` an hour later, which is the longest Google
/// accepts. A clock that is badly wrong produces an assertion Google
/// refuses with `invalid_grant`, and that refusal is shown as Google
/// worded it.
pub fn jwt_assertion(key: &ServiceAccountKey, scopes: &[&str], now: u64) -> Result<String, String> {
    let header = serde_json::json!({ "alg": "RS256", "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": key.client_email,
        "scope": scopes.join(" "),
        "aud": key.token_uri,
        "iat": now,
        "exp": now + TOKEN_LIFETIME,
    });
    let signing_input = format!(
        "{}.{}",
        b64url(header.to_string().as_bytes()),
        b64url(claims.to_string().as_bytes())
    );
    let private = RsaPrivateKey::from_pkcs8_pem(&key.private_key)
        .map_err(|e| format!("the key file's private_key is not a PKCS#8 RSA key ({})", e))?;
    let signer = SigningKey::<Sha256>::new(private);
    let signature = signer
        .try_sign(signing_input.as_bytes())
        .map_err(|e| format!("could not sign the sign-in request ({})", e))?;
    Ok(format!("{}.{}", signing_input, b64url(&signature.to_bytes())))
}

/// Where a token came from, which is what the screen says about it.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// Minted from a service-account key, and re-minted before it expires.
    KeyFile,
    /// Pasted into config as `token`.
    Config,
    /// Read from the named environment variable.
    Env(String),
}

/// An access token and when it stops working.
#[derive(Clone, PartialEq)]
pub struct Token {
    /// The bearer token itself. `Debug` prints `<redacted>` in its place.
    pub value: String,
    /// Unix seconds; `None` for a pasted token, whose expiry nobody told us.
    pub expires_at: Option<f64>,
    /// Where it came from, which decides what the screen says about it.
    pub from: Source,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("value", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("from", &self.from)
            .finish()
    }
}

/// What the screen says about a pasted token, so an expiry is not read as
/// a quiet site: `token from $VAR · Google tokens last 1h`.
pub fn pasted_token_note(from: &Source) -> Option<String> {
    match from {
        Source::KeyFile => None,
        Source::Config => Some("token from config.json · Google tokens last 1h".into()),
        Source::Env(var) => Some(format!("token from ${} · Google tokens last 1h", var)),
    }
}

/// Read the token endpoint's answer: the token and when it expires, or
/// Google's own explanation of why not.
///
/// `invalid_grant` is the one people meet - a revoked key, a deleted
/// service account, or a clock far enough off that the assertion's `iat`
/// is in Google's future - and its `error_description` is the only thing
/// that says which, so it is shown whole.
pub fn parse_token_response(status: u16, body: &str, now: f64) -> Result<(String, f64), String> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    if (200..300).contains(&status) {
        let token = v.get("access_token").and_then(|t| t.as_str()).unwrap_or("");
        if token.is_empty() {
            return Err("Google's sign-in answered without an access_token".into());
        }
        let lasts = v.get("expires_in").and_then(|e| e.as_f64()).unwrap_or(TOKEN_LIFETIME as f64);
        return Ok((token.to_string(), now + lasts));
    }
    let error = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
    let said = v.get("error_description").and_then(|e| e.as_str()).unwrap_or("");
    if error.is_empty() {
        return Err(format!("Google's sign-in refused: {}", crate::refused(status, body)));
    }
    let mut out = format!("Google refused the sign-in: {}", error);
    if !said.is_empty() {
        out.push_str(&format!(" - {}", said));
    }
    // Whole words: `exp` is inside "expired" and "expected", and a revoked
    // key ("Token has been expired or revoked.") is not a clock problem.
    let lower = said.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.iter().any(|w| matches!(*w, "iat" | "exp" | "timeframe")) {
        out.push_str(" (check this machine's clock)");
    }
    Err(out)
}

type CacheKey = (String, String, String);

static CACHE: Mutex<Option<HashMap<CacheKey, Token>>> = Mutex::new(None);

fn cache_key(key: &ServiceAccountKey, scopes: &[&str]) -> CacheKey {
    let mut sorted: Vec<&str> = scopes.to_vec();
    sorted.sort();
    (key.client_email.clone(), key.token_uri.clone(), sorted.join(" "))
}

/// The form body Google's token endpoint takes for a JWT bearer grant.
pub fn token_form(assertion: &str) -> String {
    // The assertion is base64url and dots, none of which need escaping.
    format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={}",
        assertion
    )
}

/// An access token for `scopes`, minted from `key` or taken from the cache.
///
/// The cache is per process and per (account, scopes), and a token is
/// replaced `REMINT_EARLY` seconds before it expires.
pub fn access_token(key: &ServiceAccountKey, scopes: &[&str]) -> Result<Token, String> {
    access_token_with(key, scopes, crate::now(), &mut |uri, form| {
        let (status, body, _) = crate::post_answer(
            uri,
            &[("Content-Type", "application/x-www-form-urlencoded")],
            form,
            20,
        )?;
        Ok((status, body))
    })
}

/// `access_token` with the clock and the HTTP exchange handed in, so the
/// cache can be tested without a network or a wait.
pub fn access_token_with(
    key: &ServiceAccountKey,
    scopes: &[&str],
    now: f64,
    exchange: &mut dyn FnMut(&str, &str) -> Result<(u16, String), String>,
) -> Result<Token, String> {
    let id = cache_key(key, scopes);
    {
        let cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(token) = cache.as_ref().and_then(|c| c.get(&id)) {
            if let Some(at) = token.expires_at {
                if now < at - REMINT_EARLY as f64 {
                    return Ok(token.clone());
                }
            }
        }
    }
    let assertion = jwt_assertion(key, scopes, now.max(0.0) as u64)?;
    let (status, body) = exchange(&key.token_uri, &token_form(&assertion))
        .map_err(|e| format!("could not reach Google's sign-in: {}", e))?;
    let (value, expires_at) = parse_token_response(status, &body, now)?;
    let token = Token { value, expires_at: Some(expires_at), from: Source::KeyFile };
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    cache.get_or_insert_with(HashMap::new).insert(id, token.clone());
    Ok(token)
}

/// How a widget signs in, as its config says.
///
/// `Debug` is written by hand so a pasted token never reaches a log, a
/// panic message or a test failure.
#[derive(Clone, PartialEq)]
pub enum Credential {
    /// A service-account key file, re-minted every hour indefinitely.
    KeyFile {
        /// The path as config wrote it; `~/` is expanded when it is read.
        path: String,
    },
    /// A pasted access token, good for the hour it was minted for.
    Token {
        /// The token. `Debug` prints `<redacted>` in its place.
        value: String,
        /// `token` in config, or the variable `token_env` named.
        from: Source,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::KeyFile { path } => f.debug_struct("KeyFile").field("path", path).finish(),
            Credential::Token { from, .. } => {
                f.debug_struct("Token").field("value", &"<redacted>").field("from", from).finish()
            }
        }
    }
}

/// The variable a pasted token is read from: `token_env` as config gave
/// it, or `DEFAULT_TOKEN_ENV` when that is blank.
///
/// A blank `token_env` means "the usual one", not "read no variable", so
/// a field somebody cleared on the settings screen does not quietly stop
/// a token in `$GOOGLE_OAUTH_ACCESS_TOKEN` from being read.
pub fn token_env_or_default(token_env: &str) -> &str {
    match token_env.trim() {
        "" => DEFAULT_TOKEN_ENV,
        var => var,
    }
}

/// The line every Google widget shows when its config names no
/// credential, built by [`crate::missing_config`] so it ends in
/// [`crate::SET_IN_SETTINGS`].
///
/// `section` is the widget's config section (`google_analytics`), and
/// `token_env` the variable it reads a pasted token from, as config gave
/// it; blank names `DEFAULT_TOKEN_ENV`. One wording for every Google
/// widget, so two panes side by side ask for the same thing the same way:
///
/// ```text
/// no credential: set google_analytics.credentials_file to a service-account
/// key file (docs/google-setup.md), or paste a token in google_analytics.token
/// or $GOOGLE_OAUTH_ACCESS_TOKEN — press `,` to set it here
/// ```
pub fn missing_credential(section: &str, token_env: &str) -> String {
    crate::missing_config(&format!(
        "no credential: set {section}.credentials_file to a service-account key file \
         (docs/google-setup.md), or paste a token in {section}.token or ${}",
        token_env_or_default(token_env)
    ))
}

/// Pick the credential a widget's config names, or `None` when it names
/// none - and then show `missing_credential`.
///
/// The order is the key file, then `token`, then the variable `token_env`
/// names (`DEFAULT_TOKEN_ENV` when blank). The key file wins when it is
/// set, because it is the one that keeps working; the pasted token is a
/// testing aid. A `token` in config wins over the variable, which is the
/// order every other widget here reads a token in. Pass the three
/// settings exactly as `cfg_str` read them.
pub fn credential(credentials_file: &str, token: &str, token_env: &str) -> Option<Credential> {
    credential_from(credentials_file, token, token_env, |name| std::env::var(name).ok())
}

/// `credential` with the environment handed in, for tests.
pub fn credential_from(
    credentials_file: &str,
    token: &str,
    token_env: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Option<Credential> {
    if !credentials_file.trim().is_empty() {
        return Some(Credential::KeyFile { path: credentials_file.trim().to_string() });
    }
    if !token.trim().is_empty() {
        return Some(Credential::Token { value: token.trim().to_string(), from: Source::Config });
    }
    let var = token_env_or_default(token_env);
    env(var).filter(|v| !v.trim().is_empty()).map(|value| Credential::Token {
        value: value.trim().to_string(),
        from: Source::Env(var.to_string()),
    })
}

impl Credential {
    /// A bearer token for `scopes`: minted (or cached) from the key file,
    /// or the pasted token as it is.
    pub fn bearer(&self, scopes: &[&str]) -> Result<Token, String> {
        match self {
            Credential::KeyFile { path } => access_token(&read_key_file(path)?, scopes),
            Credential::Token { value, from } => {
                Ok(Token { value: value.clone(), expires_at: None, from: from.clone() })
            }
        }
    }

    /// The service account's email, for telling somebody whom to grant
    /// access to. Unknown for a pasted token.
    pub fn client_email(&self) -> Option<String> {
        match self {
            Credential::KeyFile { path } => read_key_file(path).ok().map(|k| k.client_email),
            Credential::Token { .. } => None,
        }
    }

    /// The warning to show beside the data, if any: a key file others can
    /// read, or the reminder that a pasted token lasts an hour.
    pub fn warning(&self) -> Option<String> {
        match self {
            Credential::KeyFile { path } => key_file_warning(path),
            Credential::Token { from, .. } => pasted_token_note(from),
        }
    }
}

/// Why Google refused a request, read out of its error body.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The API is switched off in the Cloud project the key belongs to.
    NotEnabled { service: String, said: String },
    /// 403 on the resource: the account has not been given access to it.
    Denied { said: String },
    /// 429: a quota bucket is empty.
    Quota { bucket: String, reset: String, said: String },
    /// 401: the token itself was refused.
    Unauthenticated { said: String },
    /// Anything else, with Google's message.
    Other { status: u16, said: String },
}

/// Read a refusal from Google's standard error body,
/// `{"error":{"code","message","status","details":[...]}}`.
pub fn parse_refusal(status: u16, body: &str) -> Refusal {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let error = v.get("error").cloned().unwrap_or(serde_json::Value::Null);
    let said = error
        .get("message")
        .and_then(|m| m.as_str())
        .map(|m| m.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|| {
            let plain = crate::refused(status, body);
            plain.strip_prefix(&format!("HTTP {}: ", status)).unwrap_or("").to_string()
        });
    let state = error.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let details: Vec<serde_json::Value> = error
        .get("details")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let info = |field: &str| -> Option<String> {
        details.iter().find_map(|d| {
            d.get("metadata")
                .and_then(|m| m.get(field))
                .and_then(|f| f.as_str())
                .map(str::to_string)
        })
    };
    let reason = details
        .iter()
        .find_map(|d| d.get("reason").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_default();
    let disabled = reason == "SERVICE_DISABLED"
        || reason == "API_DISABLED"
        || said.contains("has not been used in project")
        || said.contains("it is disabled");
    if status == 403 && disabled {
        let service = info("service")
            .or_else(|| service_in(&said))
            .unwrap_or_else(|| "the API".into());
        return Refusal::NotEnabled { service, said };
    }
    if status == 429 || state == "RESOURCE_EXHAUSTED" {
        let bucket = info("quota_limit")
            .or_else(|| info("quota_metric"))
            .or_else(|| bucket_in(&said))
            .unwrap_or_else(|| "an unnamed bucket".into());
        let reset = reset_of(&format!("{} {}", bucket, said));
        return Refusal::Quota { bucket, reset, said };
    }
    if status == 401 || state == "UNAUTHENTICATED" {
        return Refusal::Unauthenticated { said };
    }
    if status == 403 || state == "PERMISSION_DENIED" {
        return Refusal::Denied { said };
    }
    Refusal::Other { status, said }
}

/// `analyticsdata.googleapis.com`, out of a message that names it.
fn service_in(said: &str) -> Option<String> {
    said.split(|c: char| c == '/' || c.is_whitespace() || c == '?')
        .find(|w| w.ends_with(".googleapis.com"))
        .map(str::to_string)
}

/// `property tokens per hour`, out of GA's "Exhausted property tokens per
/// hour for a property."
fn bucket_in(said: &str) -> Option<String> {
    let at = said.find("Exhausted ")? + "Exhausted ".len();
    let rest = &said[at..];
    let end = [" for ", ".", ";"]
        .iter()
        .filter_map(|stop| rest.find(stop))
        .min()
        .unwrap_or(rest.len());
    let bucket = rest[..end].trim();
    (!bucket.is_empty()).then(|| bucket.to_string())
}

/// When a bucket refills, as far as its name says.
fn reset_of(text: &str) -> String {
    let t = text.to_lowercase().replace(['-', '_'], " ");
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["per day", "perday", "daily"]) {
        "daily bucket, resets at midnight Pacific time".into()
    } else if has(&["per hour", "perhour", "hourly"]) {
        "hourly bucket, refills within the hour".into()
    } else if has(&["per minute", "perminute", "per 100 seconds"]) {
        "per-minute bucket, refills within a minute".into()
    } else if has(&["concurrent"]) {
        "frees as running requests finish".into()
    } else {
        "Google did not say when it resets".into()
    }
}

impl Refusal {
    /// One line in the wording every Google widget uses.
    ///
    /// `grant` is the widget's own remedy for a 403 on the resource -
    /// "add <email> as Viewer under Property access management" - since
    /// only the widget knows which console that is.
    pub fn wording(&self, grant: &str) -> String {
        let with = |head: String, said: &str| {
            if said.is_empty() {
                head
            } else {
                format!("{} · Google said: {}", head, said)
            }
        };
        match self {
            Refusal::NotEnabled { service, said } => with(
                format!(
                    "API not enabled: turn on {} in the key's Cloud project (APIs & Services)",
                    service
                ),
                said,
            ),
            Refusal::Denied { said } => {
                let head = if grant.is_empty() {
                    "403 no access".to_string()
                } else {
                    format!("403 no access: {}", grant)
                };
                with(head, said)
            }
            Refusal::Quota { bucket, reset, said } => {
                with(format!("429 quota: {} exhausted ({})", bucket, reset), said)
            }
            Refusal::Unauthenticated { said } => with(
                "401 the access token was refused (expired or revoked)".to_string(),
                said,
            ),
            Refusal::Other { status, said } => with(format!("HTTP {}", status), said),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};
    use rsa::signature::Verifier;
    use std::sync::OnceLock;

    /// One throwaway 2048-bit key per test run, generated here so no key
    /// material is ever in the tree. Generation is the slow part, so the
    /// tests share it.
    fn throwaway() -> &'static (RsaPrivateKey, String) {
        static KEY: OnceLock<(RsaPrivateKey, String)> = OnceLock::new();
        KEY.get_or_init(|| {
            let mut rng = rand::thread_rng();
            let private = RsaPrivateKey::new(&mut rng, 2048).expect("generate a test key");
            let pem = private.to_pkcs8_pem(LineEnding::LF).expect("encode").to_string();
            (private, pem)
        })
    }

    fn key(email: &str) -> ServiceAccountKey {
        ServiceAccountKey {
            client_email: email.into(),
            private_key: throwaway().1.clone(),
            token_uri: DEFAULT_TOKEN_URI.into(),
        }
    }

    /// PEM armour around a body that is not a key, assembled here so a
    /// secret scanner reading the tree does not take a placeholder for one.
    fn pem_shaped(body: &str) -> String {
        let label = ["PRIVATE", "KEY"].join(" ");
        format!("-----BEGIN {0}-----\n{1}\n-----END {0}-----\n", label, body)
    }

    fn decode(part: &str) -> serde_json::Value {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(part).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn the_signature_verifies_with_the_public_half() {
        let k = key("widget@example-project.iam.gserviceaccount.com");
        let jwt = jwt_assertion(&k, &[SCOPE_ANALYTICS_READONLY], 1_700_000_000)
            .unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        let public = VerifyingKey::<Sha256>::new(throwaway().0.to_public_key());
        let signed = format!("{}.{}", parts[0], parts[1]);
        public
            .verify(signed.as_bytes(), &Signature::try_from(signature.as_slice()).unwrap())
            .expect("the signature checks out against the public half");
        // And a changed claim does not.
        let forged = format!("{}.{}", parts[0], b64url(b"{\"iss\":\"someone-else\"}"));
        assert!(public
            .verify(forged.as_bytes(), &Signature::try_from(signature.as_slice()).unwrap())
            .is_err());
    }

    #[test]
    fn the_claims_name_the_account_scope_audience_and_an_hour() {
        let k = key("widget@example-project.iam.gserviceaccount.com");
        let jwt = jwt_assertion(
            &k,
            &["https://www.googleapis.com/auth/analytics.readonly", "https://example.com/b"],
            1_700_000_000,
        )
        .unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let header = decode(parts[0]);
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["typ"], "JWT");
        let claims = decode(parts[1]);
        assert_eq!(claims["iss"], "widget@example-project.iam.gserviceaccount.com");
        assert_eq!(
            claims["scope"],
            "https://www.googleapis.com/auth/analytics.readonly https://example.com/b"
        );
        assert_eq!(claims["aud"], DEFAULT_TOKEN_URI);
        assert_eq!(claims["iat"], 1_700_000_000u64);
        assert_eq!(claims["exp"], 1_700_000_000u64 + 3600);
        assert!(!jwt.contains('=') && !jwt.contains('+') && !jwt.contains('/'));
    }

    #[test]
    fn a_key_that_is_not_pkcs8_is_named_not_panicked_on() {
        let mut k = key("a@example.com");
        k.private_key = pem_shaped("not a key");
        let err = jwt_assertion(&k, &["s"], 0).unwrap_err();
        assert!(err.contains("not a PKCS#8 RSA key"), "{}", err);
    }

    #[test]
    fn a_key_file_is_parsed_and_its_mistakes_named() {
        let good = serde_json::json!({
            "type": "service_account",
            "client_email": "w@example-project.iam.gserviceaccount.com",
            "private_key": pem_shaped("AAAA"),
            "token_uri": "https://oauth2.example.com/token",
        });
        let k = parse_service_account_key(&good.to_string()).unwrap();
        assert_eq!(k.client_email, "w@example-project.iam.gserviceaccount.com");
        assert_eq!(k.token_uri, "https://oauth2.example.com/token");
        assert!(!format!("{:?}", k).contains("AAAA"), "Debug must not print the key");

        let mut no_uri = good.clone();
        no_uri.as_object_mut().unwrap().remove("token_uri");
        assert_eq!(parse_service_account_key(&no_uri.to_string()).unwrap().token_uri, DEFAULT_TOKEN_URI);

        let user = serde_json::json!({ "type": "authorized_user", "client_id": "x" });
        assert!(parse_service_account_key(&user.to_string())
            .unwrap_err()
            .contains("authorized_user credential, not a service-account key"));
        let mut no_email = good.clone();
        no_email.as_object_mut().unwrap().remove("client_email");
        assert_eq!(
            parse_service_account_key(&no_email.to_string()).unwrap_err(),
            "the key file has no client_email"
        );
        assert!(parse_service_account_key("{").unwrap_err().contains("not JSON"));
        let mut plain = good.clone();
        plain["token_uri"] = "http://example.com/token".into();
        assert!(parse_service_account_key(&plain.to_string()).unwrap_err().contains("not https"));
    }

    #[test]
    fn a_key_others_can_read_is_warned_about() {
        assert_eq!(key_mode_warning("~/k.json", 0o100600), None);
        assert_eq!(key_mode_warning("~/k.json", 0o100400), None);
        assert_eq!(
            key_mode_warning("~/k.json", 0o100644).as_deref(),
            Some("~/k.json is readable by others; chmod 600 it")
        );
        assert!(key_mode_warning("k", 0o100640).is_some());
    }

    #[test]
    fn a_token_is_cached_and_reminted_five_minutes_early() {
        // A distinct account per test so the process-wide cache is not
        // shared with another test running in parallel.
        let k = key("cache-test@example-project.iam.gserviceaccount.com");
        let scopes = [SCOPE_ANALYTICS_READONLY];
        let mut calls = 0;
        let mut exchange = |uri: &str, form: &str| -> Result<(u16, String), String> {
            calls += 1;
            assert_eq!(uri, DEFAULT_TOKEN_URI);
            assert!(form.starts_with(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion="
            ));
            Ok((200, format!("{{\"access_token\":\"t{}\",\"expires_in\":3599,\"token_type\":\"Bearer\"}}", calls)))
        };
        let t0 = 1_700_000_000.0;
        let first = access_token_with(&k, &scopes, t0, &mut exchange).unwrap();
        assert_eq!(first.value, "t1");
        assert_eq!(first.expires_at, Some(t0 + 3599.0));
        assert_eq!(first.from, Source::KeyFile);
        // Well inside the hour: the cache answers.
        let again = access_token_with(&k, &scopes, t0 + 1000.0, &mut exchange).unwrap();
        assert_eq!(again.value, "t1");
        // One second before the five-minute line: still cached.
        let edge = access_token_with(&k, &scopes, t0 + 3599.0 - 301.0, &mut exchange).unwrap();
        assert_eq!(edge.value, "t1");
        // Inside the last five minutes: a new one.
        let late = access_token_with(&k, &scopes, t0 + 3599.0 - 299.0, &mut exchange).unwrap();
        assert_eq!(late.value, "t2");
        // Scopes are their own cache entry, in any order.
        let other = access_token_with(&k, &["https://example.com/other"], t0 + 3400.0, &mut exchange).unwrap();
        assert_eq!(other.value, "t3");
        drop(exchange);
        assert_eq!(calls, 3);
    }

    #[test]
    fn a_refused_mint_is_not_cached() {
        let k = key("refused-test@example-project.iam.gserviceaccount.com");
        let mut calls = 0;
        let mut exchange = |_: &str, _: &str| -> Result<(u16, String), String> {
            calls += 1;
            Ok((400, "{\"error\":\"invalid_grant\",\"error_description\":\"Invalid JWT Signature.\"}".into()))
        };
        let a = access_token_with(&k, &["s"], 1.0e9, &mut exchange).unwrap_err();
        let b = access_token_with(&k, &["s"], 1.0e9 + 1.0, &mut exchange).unwrap_err();
        assert_eq!(a, b);
        drop(exchange);
        assert_eq!(calls, 2);
    }

    #[test]
    fn invalid_grant_shows_googles_description() {
        let err = parse_token_response(
            400,
            r#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#,
            0.0,
        )
        .unwrap_err();
        assert_eq!(err, "Google refused the sign-in: invalid_grant - Invalid JWT Signature.");
        let skew = parse_token_response(
            400,
            r#"{"error":"invalid_grant","error_description":"Invalid JWT: Token must be a short-lived token (60 minutes) and in a reasonable timeframe. Check your iat and exp values in the JWT claim."}"#,
            0.0,
        )
        .unwrap_err();
        assert!(skew.starts_with("Google refused the sign-in: invalid_grant - Invalid JWT: Token must be"));
        assert!(skew.contains("Check your iat and exp values in the JWT claim."), "whole, not capped");
        assert!(skew.ends_with("(check this machine's clock)"));
        let revoked = parse_token_response(
            400,
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
            0.0,
        )
        .unwrap_err();
        assert!(!revoked.contains("clock"), "a revoked key is not a clock problem: {}", revoked);
        let html = parse_token_response(502, "<html>Bad Gateway</html>", 0.0).unwrap_err();
        assert_eq!(html, "Google's sign-in refused: HTTP 502: <html>Bad Gateway</html>");
        let empty = parse_token_response(200, "{}", 0.0).unwrap_err();
        assert!(empty.contains("without an access_token"));
    }

    #[test]
    fn a_403_that_names_the_api_says_to_enable_it() {
        let body = r#"{"error":{"code":403,"message":"Google Analytics Data API has not been used in project 123 before or it is disabled. Enable it by visiting https://console.developers.google.com/apis/api/analyticsdata.googleapis.com/overview?project=123 then retry.","status":"PERMISSION_DENIED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"SERVICE_DISABLED","domain":"googleapis.com","metadata":{"service":"analyticsdata.googleapis.com","consumer":"projects/123"}}]}}"#;
        let r = parse_refusal(403, body);
        assert!(matches!(&r, Refusal::NotEnabled { service, .. } if service == "analyticsdata.googleapis.com"));
        let line = r.wording("add x as Viewer");
        assert!(line.starts_with(
            "API not enabled: turn on analyticsdata.googleapis.com in the key's Cloud project (APIs & Services) · Google said: Google Analytics Data API has not been used"
        ), "{}", line);
        assert!(!line.contains("Viewer"), "an API that is off is not an access problem");
        // The same refusal without the details block still finds the API.
        let bare = r#"{"error":{"code":403,"message":"Search Console API has not been used in project 9 before or it is disabled. Enable it by visiting https://console.developers.google.com/apis/api/searchconsole.googleapis.com/overview?project=9 then retry.","status":"PERMISSION_DENIED"}}"#;
        assert!(matches!(parse_refusal(403, bare), Refusal::NotEnabled { service, .. } if service == "searchconsole.googleapis.com"));
    }

    #[test]
    fn a_403_on_the_resource_carries_the_widgets_remedy() {
        let body = r#"{"error":{"code":403,"message":"User does not have sufficient permissions for this property.","status":"PERMISSION_DENIED"}}"#;
        let r = parse_refusal(403, body);
        assert!(matches!(r, Refusal::Denied { .. }));
        assert_eq!(
            r.wording("add w@example-project.iam.gserviceaccount.com as Viewer under Property access management"),
            "403 no access: add w@example-project.iam.gserviceaccount.com as Viewer under Property access management · Google said: User does not have sufficient permissions for this property."
        );
    }

    #[test]
    fn a_429_names_the_bucket_and_its_reset() {
        let ga = r#"{"error":{"code":429,"message":"Exhausted property tokens per hour for a property. Contact support to increase quota.","status":"RESOURCE_EXHAUSTED"}}"#;
        let r = parse_refusal(429, ga);
        assert_eq!(
            r,
            Refusal::Quota {
                bucket: "property tokens per hour".into(),
                reset: "hourly bucket, refills within the hour".into(),
                said: "Exhausted property tokens per hour for a property. Contact support to increase quota.".into(),
            }
        );
        assert!(r.wording("").starts_with("429 quota: property tokens per hour exhausted (hourly bucket, refills within the hour)"));
        let daily = r#"{"error":{"code":429,"message":"Exhausted property tokens per day for a property.","status":"RESOURCE_EXHAUSTED"}}"#;
        assert!(matches!(parse_refusal(429, daily), Refusal::Quota { reset, .. } if reset.contains("midnight Pacific")));
        let cloud = r#"{"error":{"code":429,"message":"Quota exceeded for quota metric 'Queries' and limit 'Queries per minute per user' of service 'searchconsole.googleapis.com'.","status":"RESOURCE_EXHAUSTED","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"RATE_LIMIT_EXCEEDED","metadata":{"quota_limit":"QueriesPerMinutePerUser","quota_metric":"searchconsole.googleapis.com/queries"}}]}}"#;
        assert!(matches!(parse_refusal(429, cloud), Refusal::Quota { bucket, reset, .. }
            if bucket == "QueriesPerMinutePerUser" && reset.contains("per-minute")));
        let unnamed = parse_refusal(429, "{}");
        assert!(matches!(unnamed, Refusal::Quota { bucket, reset, .. }
            if bucket == "an unnamed bucket" && reset == "Google did not say when it resets"));
    }

    #[test]
    fn a_401_and_anything_else_keep_googles_words() {
        let r = parse_refusal(401, r#"{"error":{"code":401,"message":"Request had invalid authentication credentials.","status":"UNAUTHENTICATED"}}"#);
        assert_eq!(
            r.wording(""),
            "401 the access token was refused (expired or revoked) · Google said: Request had invalid authentication credentials."
        );
        let r = parse_refusal(500, "upstream connect error");
        assert_eq!(r.wording(""), "HTTP 500 · Google said: upstream connect error");
    }

    #[test]
    fn the_credential_is_chosen_in_order_and_a_pasted_token_says_it_lasts_an_hour() {
        let env = |name: &str| (name == "GOOGLE_OAUTH_ACCESS_TOKEN").then(|| "pasted".to_string());
        assert_eq!(
            credential_from("~/k.json", "tok", "GOOGLE_OAUTH_ACCESS_TOKEN", env),
            Some(Credential::KeyFile { path: "~/k.json".into() })
        );
        assert_eq!(
            credential_from("", "tok", "GOOGLE_OAUTH_ACCESS_TOKEN", env),
            Some(Credential::Token { value: "tok".into(), from: Source::Config })
        );
        let pasted = credential_from("", "", "GOOGLE_OAUTH_ACCESS_TOKEN", env).unwrap();
        assert_eq!(
            pasted,
            Credential::Token {
                value: "pasted".into(),
                from: Source::Env("GOOGLE_OAUTH_ACCESS_TOKEN".into())
            }
        );
        assert_eq!(
            pasted.warning().as_deref(),
            Some("token from $GOOGLE_OAUTH_ACCESS_TOKEN · Google tokens last 1h")
        );
        let bearer = pasted.bearer(&["s"]).unwrap();
        assert_eq!(bearer.value, "pasted");
        assert_eq!(bearer.expires_at, None);
        assert!(!format!("{:?}", bearer).contains("pasted"), "Debug must not print the token");
        assert!(!format!("{:?}", pasted).contains("pasted"), "nor the credential holding it");
        assert!(format!("{:?}", pasted).contains("<redacted>"));
        assert_eq!(credential_from("", "", "OTHER_VAR", env), None);
        // A blank token_env is the usual variable, not no variable.
        assert_eq!(
            credential_from(" ", " ", "", env),
            Some(Credential::Token {
                value: "pasted".into(),
                from: Source::Env("GOOGLE_OAUTH_ACCESS_TOKEN".into())
            })
        );
        assert_eq!(credential_from("", "", " ", |_: &str| None), None);
    }

    #[test]
    fn a_missing_credential_is_worded_once_for_every_google_widget() {
        assert_eq!(
            missing_credential("google_analytics", "GOOGLE_OAUTH_ACCESS_TOKEN"),
            "no credential: set google_analytics.credentials_file to a service-account key file \
             (docs/google-setup.md), or paste a token in google_analytics.token or \
             $GOOGLE_OAUTH_ACCESS_TOKEN — press `,` to set it here"
        );
        // A blank token_env names the variable that is actually read.
        assert_eq!(
            missing_credential("google_search_console", " "),
            missing_credential("google_search_console", DEFAULT_TOKEN_ENV)
        );
        assert!(missing_credential("s", "MY_VAR").contains("s.token or $MY_VAR — "));
        assert!(missing_credential("s", "").ends_with(crate::SET_IN_SETTINGS));
    }

    #[test]
    fn the_scopes_are_the_read_only_ones() {
        assert_eq!(SCOPE_ANALYTICS_READONLY, "https://www.googleapis.com/auth/analytics.readonly");
        assert_eq!(SCOPE_WEBMASTERS_READONLY, "https://www.googleapis.com/auth/webmasters.readonly");
        assert!([SCOPE_ANALYTICS_READONLY, SCOPE_WEBMASTERS_READONLY]
            .iter()
            .all(|s| s.ends_with(".readonly")));
    }

    #[test]
    fn a_missing_key_file_says_which_path() {
        let err = read_key_file("/nonexistent/opscope-test-key.json").unwrap_err();
        assert!(err.starts_with("cannot read the key file /nonexistent/opscope-test-key.json"), "{}", err);
    }
}
