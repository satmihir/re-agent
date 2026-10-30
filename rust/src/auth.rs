use crate::transport::http::Client;
use crate::types::json_string;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Explains how to reconnect an unusable ChatGPT authorization.
pub const RECONNECT: &str = "Your ChatGPT connection is no longer valid; run reagent login.";
const RESOURCE: &str = "https://api.openai.com/v1";
const UNKNOWN: &str = "refresh_outcome_unknown";
static SECRETS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static TOKEN_LOCK: Mutex<()> = Mutex::new(());
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Credentials {
    host_id: String,
    client_id: String,
    sub: String,
    email: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    id_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    access_token: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    refresh_token: String,
    #[serde(skip_serializing_if = "is_zero")]
    expires_at: i64,
    #[serde(skip_serializing_if = "is_zero")]
    earliest_refresh_at: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    scope: String,
    #[serde(skip_serializing_if = "is_false")]
    needs_login: bool,
    #[serde(skip_serializing_if = "is_zero_u64")]
    logout_generation: u64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}
fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn granted(c: &Credentials) -> bool {
    c.scope
        .split_whitespace()
        .any(|s| s == "chatgpt.tokens.use.direct")
}

fn stable(error: std::io::Error) -> String {
    crate::errors::stable_io(&error).into()
}

/// Returns the private ChatGPT credential file location.
pub fn credential_path() -> Result<PathBuf, String> {
    let root = if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .filter(|s| !s.is_empty())
            .map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|s| !s.is_empty())
                    .map(|h| PathBuf::from(h).join(".config"))
            })
    };
    root.map(|p| p.join("reagent/chatgpt.json"))
        .ok_or_else(|| "user configuration directory is unavailable".into())
}

fn private_directory(path: &Path, create: bool) -> Result<(), String> {
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|e| format!("create session directory: {}", stable(e)))?;
    }
    let info = fs::symlink_metadata(path)
        .map_err(|e| format!("inspect session directory: {}", stable(e)))?;
    if !info.is_dir() || info.permissions().mode() & 0o777 != 0o700 {
        return Err(format!(
            "session directory {} must be private (0700) and not a symlink",
            path.display()
        ));
    }
    Ok(())
}

fn read_credentials() -> Result<Credentials, String> {
    let path = credential_path()?;
    let dir = path.parent().unwrap();
    if fs::symlink_metadata(dir).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(Credentials::default());
    }
    private_directory(dir, false).map_err(|e| format!("open ChatGPT credential directory: {e}"))?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Credentials::default()),
        Err(e) => return Err(format!("open ChatGPT credentials: {}", stable(e))),
    };
    let info = file
        .metadata()
        .map_err(|e| format!("inspect ChatGPT credentials: {}", stable(e)))?;
    if !info.is_file() || info.permissions().mode() & 0o777 != 0o600 {
        return Err("ChatGPT credentials must be a private regular file (0600)".into());
    }
    let mut deserializer = serde_json::Deserializer::from_reader(file.take(1 << 20));
    let value = Value::deserialize(&mut deserializer)
        .map_err(|_| "ChatGPT credentials are corrupt; run reagent login")?;
    let c = if value.is_null() {
        Credentials::default()
    } else {
        serde_json::from_value(value)
            .map_err(|_| "ChatGPT credentials are corrupt; run reagent login")?
    };
    remember(&c);
    Ok(c)
}

fn save_credentials(c: &Credentials) -> Result<(), String> {
    let path = credential_path()?;
    let dir = path.parent().unwrap();
    private_directory(dir, true).map_err(|e| format!("open credential directory: {e}"))?;
    let stage = dir.join(format!(".chatgpt-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stage)
            .map_err(|e| format!("stage ChatGPT credentials: {}", stable(e)))?;
        let value =
            serde_json::to_value(c).map_err(|_| "write ChatGPT credentials: invalid JSON")?;
        file.write_all(format!("{}\n", json_string(&value)).as_bytes())
            .map_err(|e| format!("write ChatGPT credentials: {}", stable(e)))?;
        file.sync_all()
            .map_err(|e| format!("sync ChatGPT credentials: {}", stable(e)))?;
        drop(file);
        fs::rename(&stage, &path)
            .map_err(|e| format!("replace ChatGPT credentials: {}", stable(e)))?;
        File::open(dir)
            .map_err(|e| format!("open ChatGPT credential directory: {}", stable(e)))?
            .sync_all()
            .map_err(|e| format!("sync ChatGPT credential directory: {}", stable(e)))?;
        Ok(())
    })();
    let _ = fs::remove_file(stage);
    result
}

struct CredentialLock(File);
impl Drop for CredentialLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock_credentials(deadline: Instant) -> Result<CredentialLock, String> {
    lock_credentials_cancelable(deadline, None)
}
fn lock_credentials_cancelable(
    deadline: Instant,
    cancellation: Option<&AtomicBool>,
) -> Result<CredentialLock, String> {
    let path = credential_path()?;
    private_directory(path.parent().unwrap(), true)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path.with_extension("json.lock"))
        .map_err(|e| format!("open ChatGPT credential lock: {}", stable(e)))?;
    loop {
        if cancellation.is_some_and(|token| token.load(Ordering::SeqCst)) {
            return Err("lock ChatGPT credentials: operation cancelled".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(CredentialLock(file));
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK)) {
            return Err(format!("lock ChatGPT credentials: {}", stable(error)));
        }
        if Instant::now() >= deadline || INTERRUPTED.load(Ordering::SeqCst) {
            return Err("lock ChatGPT credentials: operation timed out".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn remember(c: &Credentials) {
    let mut secrets = SECRETS.lock().unwrap_or_else(|p| p.into_inner());
    for s in [&c.access_token, &c.refresh_token, &c.id_token] {
        if !s.is_empty() && !secrets.contains(s) {
            secrets.push(s.clone());
        }
    }
}

/// Loads credentials without contacting an authorization service.
pub fn load() -> Result<Value, String> {
    serde_json::to_value(read_credentials()?)
        .map_err(|_| "cannot encode ChatGPT credentials".into())
}

/// Reports whether saved credentials can back a ChatGPT token source.
pub fn usable() -> bool {
    read_credentials().is_ok_and(|c| valid_source(&c))
}

fn valid_source(c: &Credentials) -> bool {
    granted(c)
        && !c.needs_login
        && !c.access_token.is_empty()
        && !c.refresh_token.is_empty()
        && !c.client_id.is_empty()
}

/// Returns remembered retired and current credential secrets for redaction.
pub fn secrets() -> Vec<String> {
    let _ = read_credentials();
    SECRETS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn issuer() -> String {
    endpoint("REAGENT_OPENAI_AUTH_ISSUER", "https://auth.openai.com")
}
fn endpoint(variable: &str, fallback: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| fallback.into())
}

fn client(timeout: Duration) -> Result<Client, String> {
    let header = crate::transport::timeout("REAGENT_RESPONSE_HEADER_TIMEOUT", 600)?;
    let idle = crate::transport::timeout("REAGENT_RESPONSE_IDLE_TIMEOUT", 300)?;
    let mut client = Client::new(
        Duration::from_secs(header),
        Duration::from_secs(idle),
        Some(timeout),
    )
    .map_err(|_| "could not contact ChatGPT authorization service")?;
    client.interruption = Some(&INTERRUPTED);
    Ok(client)
}

enum AuthError {
    Cancelled,
    OAuth(String),
    Transport(Option<u16>),
    InvalidEndpoint,
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("context canceled"),
            Self::OAuth(_) => f.write_str("ChatGPT authorization failed; run reagent login."),
            Self::Transport(Some(status)) => write!(
                f,
                "ChatGPT authorization service temporarily unavailable (HTTP {status})"
            ),
            Self::Transport(None) => f.write_str("could not contact ChatGPT authorization service"),
            Self::InvalidEndpoint => f.write_str("invalid ChatGPT authorization endpoint"),
        }
    }
}

fn terminal(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant" | "invalid_refresh_token" | "token_expired"
    ) || code.starts_with("refresh_token_")
}

fn form(
    client: &Client,
    endpoint: &str,
    fields: &[(&str, &str)],
    decode: bool,
) -> Result<Value, AuthError> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| AuthError::InvalidEndpoint)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AuthError::InvalidEndpoint);
    }
    let fields: BTreeMap<_, _> = fields.iter().copied().collect();
    let response = client.post(url).form(&fields).send().map_err(|e| {
        if e.kind() == std::io::ErrorKind::ConnectionAborted {
            AuthError::Cancelled
        } else if e.kind() == std::io::ErrorKind::ConnectionRefused {
            AuthError::Transport(None)
        } else {
            AuthError::OAuth(UNKNOWN.into())
        }
    })?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) && !decode {
        return Ok(Value::Null);
    }
    let mut raw = Vec::new();
    response
        .take(1 << 20)
        .read_to_end(&mut raw)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::ConnectionAborted {
                AuthError::Cancelled
            } else {
                AuthError::OAuth(UNKNOWN.into())
            }
        })?;
    let mut de = serde_json::Deserializer::from_slice(&raw);
    let body = Value::deserialize(&mut de);
    if !(200..300).contains(&status) {
        let code = body
            .ok()
            .map(|v| {
                v["error"]
                    .as_str()
                    .unwrap_or_else(|| text(&v["error"], "code"))
                    .to_owned()
            })
            .unwrap_or_default();
        if (status == 429 || (500..=599).contains(&status)) && !terminal(&code) {
            return Err(AuthError::Transport(Some(status)));
        }
        return Err(AuthError::OAuth(code));
    }
    body.map_err(|_| AuthError::OAuth(UNKNOWN.into()))
}

fn apply_tokens(c: &mut Credentials, tokens: &Value) -> Result<(), String> {
    let access = text(tokens, "access_token");
    let refresh = text(tokens, "refresh_token");
    let expires = tokens["expires_in"].as_i64().unwrap_or(0);
    if access.is_empty()
        || refresh.is_empty()
        || !text(tokens, "token_type").eq_ignore_ascii_case("Bearer")
        || expires <= 0
    {
        return Err("incomplete ChatGPT token response; run reagent login".into());
    }
    c.access_token = access.into();
    c.refresh_token = refresh.into();
    if !text(tokens, "id_token").is_empty() {
        c.id_token = text(tokens, "id_token").into();
    }
    c.expires_at = now().wrapping_add(expires);
    c.earliest_refresh_at = tokens["earliest_refresh_at"].as_i64().unwrap_or(0);
    if !text(tokens, "scope").is_empty() {
        c.scope = text(tokens, "scope").into();
    }
    c.needs_login = false;
    remember(c);
    Ok(())
}

fn cached(c: &Credentials, force: bool, rejected: &str) -> Result<Option<String>, String> {
    if !valid_source(c) {
        return Err(RECONNECT.into());
    }
    let now = now();
    if force && !rejected.is_empty() && c.access_token != rejected && c.expires_at > now {
        return Ok(Some(c.access_token.clone()));
    }
    if !force && c.expires_at.saturating_sub(now) > 120 {
        return Ok(Some(c.access_token.clone()));
    }
    if now < c.earliest_refresh_at {
        if !force && c.expires_at > now {
            return Ok(Some(c.access_token.clone()));
        }
        return Err(RECONNECT.into());
    }
    Ok(None)
}

/// Gets a cached or durably refreshed token, optionally replacing a rejected token.
pub fn token(force: bool, rejected: &str) -> Result<String, String> {
    token_cancelable(force, rejected, None)
}

/// token_cancelable cancels credential waits but durably completes any started rotation.
pub fn token_cancelable(
    force: bool,
    rejected: &str,
    cancellation: Option<Arc<AtomicBool>>,
) -> Result<String, String> {
    token_until(force, rejected, cancellation, None)
}
pub(crate) fn token_until(
    force: bool,
    rejected: &str,
    cancellation: Option<Arc<AtomicBool>>,
    deadline: Option<Instant>,
) -> Result<String, String> {
    let _guard = loop {
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Err("refresh ChatGPT credentials: operation timed out".into());
        }
        if cancellation
            .as_ref()
            .is_some_and(|token| token.load(Ordering::SeqCst))
        {
            return Err("refresh ChatGPT credentials: operation cancelled".into());
        }
        match TOKEN_LOCK.try_lock() {
            Ok(guard) => break guard,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(20))
            }
        }
    };
    if let Some(token) = cached(&read_credentials()?, force, rejected)? {
        return Ok(token);
    }
    let _lock = lock_credentials_cancelable(
        deadline.map_or(Instant::now() + Duration::from_secs(300), |end| {
            end.min(Instant::now() + Duration::from_secs(300))
        }),
        cancellation.as_deref(),
    )?;
    let mut c = read_credentials()?;
    if let Some(token) = cached(&c, force, rejected)? {
        return Ok(token);
    }
    if cancellation
        .as_ref()
        .is_some_and(|token| token.load(Ordering::SeqCst))
    {
        return Err("refresh ChatGPT credentials: operation cancelled".into());
    }
    if deadline.is_some_and(|end| Instant::now() >= end) {
        return Err("refresh ChatGPT credentials: operation timed out".into());
    }
    // A rotating token's replacement must be persisted even if the caller cancels.
    let mut client = client(Duration::from_secs(30))?;
    client.interruption = None;
    let result = form(
        &client,
        &format!("{}/api/accounts/oauth/token", issuer()),
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &c.client_id),
            ("refresh_token", &c.refresh_token),
            ("resource", RESOURCE),
        ],
        true,
    )
    .and_then(|tokens| apply_tokens(&mut c, &tokens).map_err(|_| AuthError::OAuth(UNKNOWN.into())));
    if let Err(AuthError::OAuth(code)) = &result {
        if terminal(code) || code == UNKNOWN {
            c.needs_login = true;
            if code != UNKNOWN {
                c.access_token.clear();
                c.refresh_token.clear();
            }
            save_credentials(&c)?;
            return Err(RECONNECT.into());
        }
    }
    result.map_err(|e| e.to_string())?;
    save_credentials(&c)?;
    if !granted(&c) {
        return Err("ChatGPT plan usage was not granted; run reagent login.".into());
    }
    Ok(c.access_token)
}

/// Builds bearer headers for the configured provider or ChatGPT connection.
pub fn headers(config: &Value, _request: &Value) -> Result<reqwest::header::HeaderMap, String> {
    let bearer = if text(config, "auth_mode") == "chatgpt" {
        token(false, "")?
    } else {
        let variable = if text(config, "provider") == "anthropic" {
            "ANTHROPIC_API_KEY"
        } else {
            "OPENAI_API_KEY"
        };
        std::env::var(variable).unwrap_or_default()
    };
    let mut headers = reqwest::header::HeaderMap::new();
    if !bearer.is_empty() {
        if text(config, "provider") == "anthropic" {
            headers.insert(
                "x-api-key",
                reqwest::header::HeaderValue::from_str(&bearer).map_err(|_| "invalid API key")?,
            );
        } else {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {bearer}"))
                    .map_err(|_| "invalid API key")?,
            );
        }
    }
    Ok(headers)
}

fn random() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("generate ChatGPT authorization randomness: {}", stable(e)))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn discover(client: &Client) -> Result<Value, String> {
    let issuer = issuer();
    let base = reqwest::Url::parse(&issuer).map_err(|_| "invalid ChatGPT authorization issuer")?;
    if base.host_str().is_none() || !matches!(base.scheme(), "http" | "https") {
        return Err("invalid ChatGPT authorization issuer".into());
    }
    let response = client
        .get(format!("{issuer}/.well-known/openid-configuration"))
        .send()
        .map_err(|_| "could not discover ChatGPT authorization service")?;
    if response.status().as_u16() != 200 {
        return Err("invalid ChatGPT authorization discovery response".into());
    }
    let mut de = serde_json::Deserializer::from_reader(response.take(1 << 20));
    let mut d = Value::deserialize(&mut de)
        .map_err(|_| "invalid ChatGPT authorization discovery response")?;
    if !d.is_object() {
        return Err("invalid ChatGPT authorization discovery response".into());
    }
    if text(&d, "issuer").is_empty() {
        d["issuer"] = json!(issuer);
    }
    for field in ["issuer", "authorization_endpoint", "token_endpoint"] {
        let u = reqwest::Url::parse(text(&d, field))
            .map_err(|_| "ChatGPT discovery endpoint does not match the configured issuer")?;
        if u.host_str() != base.host_str()
            || u.port() != base.port()
            || u.scheme() != base.scheme()
            || !u.username().is_empty()
            || u.password().is_some()
        {
            return Err("ChatGPT discovery endpoint does not match the configured issuer".into());
        }
    }
    Ok(d)
}

fn constant_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |different, (x, y)| different | (x ^ y))
        == 0
}

fn validate_id_token(
    token: &str,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    subject: &str,
) -> Result<(String, String), String> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("invalid ChatGPT ID token".into());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| "invalid ChatGPT ID token claims")?;
    let c: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid ChatGPT ID token claims")?;
    let audience = c["aud"].as_str().is_some_and(|a| a == client_id)
        || c["aud"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a.as_str() == Some(client_id)));
    if text(&c, "iss") != issuer
        || !audience
        || c["exp"].as_i64().unwrap_or(0) <= now()
        || !constant_equal(text(&c, "nonce"), nonce)
        || text(&c, "sub").is_empty()
    {
        return Err("ChatGPT ID token claims do not match this login".into());
    }
    if !subject.is_empty() && text(&c, "sub") != subject {
        return Err(format!("ChatGPT login must use the same account; use the saved ChatGPT account or OPENAI_API_KEY (saved registration: {})",credential_path()?.display()));
    }
    Ok((text(&c, "sub").into(), text(&c, "email").into()))
}

fn callback(
    address: &str,
    redirect: &str,
    state: &str,
    client_id: &str,
) -> Result<BTreeMap<String, String>, String> {
    let url = reqwest::Url::parse(address)
        .map_err(|_| "paste the complete loopback callback URL from this login")?;
    let base = format!("{}{}", url.origin().ascii_serialization(), url.path());
    if base != redirect
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("paste the complete loopback callback URL from this login".into());
    }
    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, v) in url.query_pairs() {
        values
            .entry(k.into_owned())
            .or_default()
            .push(v.into_owned());
    }
    let get = |key: &str| {
        values
            .get(key)
            .and_then(|a| a.first())
            .map(String::as_str)
            .unwrap_or("")
    };
    let count = |key: &str| values.get(key).map_or(0, Vec::len);
    if count("state") != 1 || !constant_equal(get("state"), state) {
        return Err("ChatGPT login state did not match this attempt".into());
    }
    if count("error") > 1 {
        return Err("incomplete ChatGPT login callback".into());
    }
    if !get("error").is_empty() {
        return Ok(BTreeMap::from([("error".into(), get("error").into())]));
    }
    if count("code") != 1 || get("code").is_empty() || count("client_id") > 1 {
        return Err("incomplete ChatGPT login callback".into());
    }
    let issued = get("client_id");
    if client_id.is_empty() && (!issued.starts_with("oaiapp_") || issued.len() == "oaiapp_".len()) {
        return Err("ChatGPT callback did not issue a valid client ID".into());
    }
    if !client_id.is_empty() && !issued.is_empty() && issued != client_id {
        return Err("ChatGPT callback returned a different client ID".into());
    }
    Ok(values
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().next().unwrap_or_default()))
        .collect())
}

fn open_browser(address: &str) -> bool {
    let executable = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let Ok(mut child) = Command::new(executable)
        .arg(address)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Err(_) => return false,
            _ => {}
        }
        if started.elapsed() >= Duration::from_secs(10) || INTERRUPTED.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_callback(
    listener: &TcpListener,
    redirect: &str,
    state: &str,
    client_id: &str,
    authorize: &str,
    output: &mut dyn Write,
    deadline: Instant,
) -> Result<BTreeMap<String, String>, String> {
    listener
        .set_nonblocking(true)
        .map_err(|_| "could not listen for ChatGPT callback")?;
    let mut shown =
        reqwest::Url::parse(authorize).map_err(|_| "invalid ChatGPT authorization endpoint")?;
    let hinted = shown.query_pairs().any(|(k, _)| k == "id_token_hint");
    let fields: BTreeMap<String, String> = shown
        .query_pairs()
        .filter(|(k, _)| k != "id_token_hint")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    shown.set_query(None);
    shown.query_pairs_mut().extend_pairs(fields);
    if !shown.username().is_empty() {
        let _ = shown.set_username("xxxxx");
    }
    if shown.password().is_some() {
        let _ = shown.set_password(Some("xxxxx"));
    }
    let port = listener
        .local_addr()
        .map_err(|_| "could not listen for ChatGPT callback")?
        .port();
    let _ = writeln!(output,"Opening your browser for ChatGPT sign-in.\n{shown}\nWaiting for approval (Ctrl-C cancels). If the callback cannot reach this machine, paste the final {redirect}?... URL here, or use ssh -L {port}:127.0.0.1:{port}.");
    if hinted {
        let _ = writeln!(
            output,
            "The printed URL omits the saved identity token and may show the account picker."
        );
    }
    let _ = output.flush();
    if !open_browser(authorize) {
        let _ = writeln!(
            output,
            "Could not open the browser; open the URL above yourself."
        );
    }
    let mut line = Vec::new();
    let mut stdin_open = true;
    loop {
        if Instant::now() >= deadline || INTERRUPTED.load(Ordering::SeqCst) {
            return Err("ChatGPT sign-in cancelled or timed out".into());
        }
        if let Ok((mut stream, _)) = listener.accept() {
            // Darwin inherits the listener's O_NONBLOCK flag; a read timeout alone does not clear it.
            stream
                .set_nonblocking(false)
                .map_err(|_| "could not read ChatGPT callback")?;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut bytes = Vec::new();
            let mut byte = [0u8; 1];
            while bytes.len() < 65536 {
                match stream.read(&mut byte) {
                    Ok(1) => bytes.push(byte[0]),
                    _ => break,
                }
                if bytes.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let raw = String::from_utf8_lossy(&bytes);
            let mut lines = raw.split("\r\n");
            let first: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
            let host = lines
                .find_map(|s| {
                    s.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
                        .map(|(_, v)| v.trim())
                })
                .unwrap_or("");
            let result = if first.first() == Some(&"GET") && first.len() == 3 {
                callback(
                    &format!("http://{host}{}", first[1]),
                    redirect,
                    state,
                    client_id,
                )
            } else {
                Err("invalid ChatGPT login callback".into())
            };
            let (status, body) = match &result {
                Ok(_) => (
                    "200 OK",
                    "Return to re:agent — you can close this tab.\n".to_owned(),
                ),
                Err(e) => ("400 Bad Request", format!("{e}\n")),
            };
            let _ = write!(stream,"HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            if let Ok(values) = result {
                return approved(values);
            }
        }
        if stdin_open {
            if matches!(crate::terminal::poll_read(0, 0), Ok(events) if events & (libc::POLLIN | libc::POLLHUP) != 0)
            {
                let mut byte = [0u8; 1];
                let read = unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) };
                if read <= 0 {
                    stdin_open = false;
                }
                if read > 0 {
                    line.push(byte[0]);
                }
                if byte[0] == b'\n' || !stdin_open {
                    let input = String::from_utf8_lossy(&line);
                    if !input.trim().is_empty() {
                        match callback(input.trim(), redirect, state, client_id) {
                            Ok(values) => return approved(values),
                            Err(error) => {
                                let _ = writeln!(output,"Invalid callback: {error}. Still waiting for approval; paste the final callback URL or press Ctrl-C to cancel.");
                            }
                        }
                    }
                    line.clear();
                }
                if line.len() > 1 << 20 {
                    stdin_open = false;
                    line.clear();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn approved(values: BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    if values.contains_key("error") {
        Err("ChatGPT sign-in was not approved; run reagent login again".into())
    } else {
        Ok(values)
    }
}

/// Runs browser-based authorization with PKCE and a validated loopback callback.
pub fn login(output: &mut dyn Write) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut c = {
        let _lock = lock_credentials(deadline)?;
        let mut c = read_credentials()?;
        if c.host_id.is_empty() {
            c.host_id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
            save_credentials(&c)?;
        }
        c
    };
    let client = client(Duration::from_secs(30))?;
    let discovery = discover(&client)?;
    let listener = TcpListener::bind("127.0.0.1:1455")
        .or_else(|_| TcpListener::bind("127.0.0.1:0"))
        .map_err(|e| format!("could not listen for ChatGPT callback: {}", stable(e)))?;
    let redirect = format!(
        "http://{}/auth/callback",
        listener.local_addr().map_err(|e| stable(e))?
    );
    let (state, nonce, verifier) = (random()?, random()?, random()?);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let client_id = if c.client_id.is_empty() {
        "dynamic_agent_client"
    } else {
        &c.client_id
    };
    let mut fields = BTreeMap::from([
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect.as_str()),
        (
            "scope",
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
        ),
        ("resource", RESOURCE),
        ("state", &state),
        ("nonce", &nonce),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("ext_agent_host_id", &c.host_id),
    ]);
    if c.client_id.is_empty() {
        fields.insert("agent_name_hint", "re:agent");
    } else {
        if !c.id_token.is_empty() {
            fields.insert("id_token_hint", &c.id_token);
        }
        if !granted(&c) {
            fields.insert("prompt", "consent");
        }
        if !c.email.is_empty() {
            fields.insert("login_hint", &c.email);
        }
    }
    let mut authorize = reqwest::Url::parse(text(&discovery, "authorization_endpoint"))
        .map_err(|_| "invalid ChatGPT authorization endpoint")?;
    authorize.set_query(None);
    authorize.query_pairs_mut().extend_pairs(fields);
    let callback = wait_callback(
        &listener,
        &redirect,
        &state,
        &c.client_id,
        authorize.as_str(),
        output,
        deadline,
    )?;
    let _lock = lock_credentials(deadline)?;
    let current = read_credentials()?;
    if current.host_id != c.host_id
        || current.client_id != c.client_id
        || current.sub != c.sub
        || current.logout_generation != c.logout_generation
    {
        return Err("ChatGPT credentials changed during sign-in; run reagent login again".into());
    }
    c = current;
    if c.client_id.is_empty() {
        c.client_id = callback.get("client_id").cloned().unwrap_or_default();
        save_credentials(&c)?;
    }
    let code = callback.get("code").map(String::as_str).unwrap_or("");
    let tokens = form(
        &client,
        text(&discovery, "token_endpoint"),
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &c.client_id),
            ("code", code),
            ("code_verifier", &verifier),
            ("redirect_uri", &redirect),
            ("resource", RESOURCE),
        ],
        true,
    )
    .map_err(|e| e.to_string())?;
    let (subject, email) = validate_id_token(
        text(&tokens, "id_token"),
        text(&discovery, "issuer"),
        &c.client_id,
        &nonce,
        &c.sub,
    )?;
    c.scope.clear();
    apply_tokens(&mut c, &tokens)?;
    c.sub = subject;
    c.email = email;
    save_credentials(&c)?;
    serde_json::to_value(c).map_err(|_| "cannot encode ChatGPT credentials".into())
}

/// Revokes saved refresh credentials and invalidates pending sign-ins.
pub fn logout(output: &mut dyn Write) -> Result<(), String> {
    let _lock = lock_credentials(Instant::now() + Duration::from_secs(300))?;
    let mut c = read_credentials()?;
    if !c.refresh_token.is_empty()
        && form(
            &client(Duration::from_secs(30))?,
            &format!("{}/api/accounts/oauth/revoke", issuer()),
            &[
                ("token", &c.refresh_token),
                ("token_type_hint", "refresh_token"),
                ("client_id", &c.client_id),
            ],
            false,
        )
        .is_err()
    {
        let _ = writeln!(output,"Could not confirm revocation. Disconnect re:agent in https://chatgpt.com/settings before signing in again.");
    }
    c.id_token.clear();
    c.access_token.clear();
    c.refresh_token.clear();
    c.scope.clear();
    c.expires_at = 0;
    c.earliest_refresh_at = 0;
    c.needs_login = false;
    c.logout_generation = c.logout_generation.wrapping_add(1);
    save_credentials(&c)?;
    let _ = writeln!(output, "Signed out of ChatGPT.");
    Ok(())
}

fn visible(text: &str) -> String {
    let mut text = crate::transport::redact(text, &[], &issuer());
    text = crate::transport::redact(
        &text,
        &[],
        &endpoint("REAGENT_OPENAI_BASE_URL", "https://api.openai.com/v1"),
    );
    for secret in secrets() {
        text = text.replace(&secret, "[redacted]");
    }
    text.chars()
        .map(|c| match c {
            '\n' => "\\n".into(),
            '\t' => "\\t".into(),
            c if c.is_control() => format!("\\x{:02x}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

fn print_models(token: &str, output: &mut dyn Write) -> Result<(), String> {
    let response = client(Duration::from_secs(30))?
        .get(format!(
            "{}/models",
            endpoint("REAGENT_OPENAI_BASE_URL", "https://api.openai.com/v1")
        ))
        .bearer_auth(token)
        .send()
        .map_err(|_| "could not list ChatGPT models")?;
    let status = response.status().as_u16();
    let mut de = serde_json::Deserializer::from_reader(response.take(1 << 20));
    let body = Value::deserialize(&mut de);
    if status != 200 || body.is_err() {
        return Err(format!("could not list ChatGPT models (HTTP {status})"));
    }
    let body = body.unwrap();
    let _ = writeln!(output, "Available models:");
    if let Some(models) = body["models"].as_array() {
        for model in models {
            if text(model, "visibility") == "list" {
                let _ = writeln!(
                    output,
                    "  {} ({})",
                    visible(text(model, "slug")),
                    visible(text(model, "display_name"))
                );
            }
        }
    }
    Ok(())
}

extern "C" fn interrupt(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}
struct InterruptHandler(libc::sighandler_t);
impl Drop for InterruptHandler {
    fn drop(&mut self) {
        unsafe {
            libc::signal(libc::SIGINT, self.0);
        }
        INTERRUPTED.store(false, Ordering::SeqCst);
    }
}

/// Implements login, logout and auth status with the command-line exit contract.
pub fn command(args: &[String]) -> i32 {
    let mut output = std::io::stdout();
    let mut error = std::io::stderr();
    INTERRUPTED.store(false, Ordering::SeqCst);
    let _signal = InterruptHandler(unsafe {
        libc::signal(libc::SIGINT, interrupt as *const () as libc::sighandler_t)
    });
    if let Err(e) = crate::provider::validate_environment() {
        let _ = writeln!(error, "error: {e}");
        return 2;
    }
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["logout"] => logout(&mut output),
        ["login"] => login(&mut error).and_then(|v| {
            let c: Credentials = serde_json::from_value(v).map_err(|_|"invalid ChatGPT credentials")?;
            if !granted(&c) {
                let _ = writeln!(output,"Signed in, but ChatGPT plan usage was not granted. Run reagent login again and allow it, or set OPENAI_API_KEY.");
                return Ok(());
            }
            let _ = writeln!(output,"Signed in as {}. ChatGPT plan usage: enabled.",visible(&c.email));
            if let Err(e) = print_models(&c.access_token,&mut output) { let _ = writeln!(error,"{e}"); }
            Ok(())
        }),
        ["auth","status"] => status(&mut output),
        _ => { let _ = writeln!(error,"Usage: reagent login | reagent logout | reagent auth status"); return 2; }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(error, "error: {e}");
            2
        }
    }
}

fn status(output: &mut dyn Write) -> Result<(), String> {
    let mut c = read_credentials()?;
    if c.access_token.is_empty() || c.needs_login {
        let account = if c.email.is_empty() {
            "none".into()
        } else {
            visible(&c.email)
        };
        let _ = writeln!(output,"Account: {account}\nChatGPT plan usage: disabled\nAccess token expires: none\nNot signed in to ChatGPT. Run reagent login.");
        return Ok(());
    }
    let grant = if granted(&c) {
        token(false, "")?;
        c = read_credentials()?;
        "enabled"
    } else {
        "not granted"
    };
    let expires = chrono::DateTime::from_timestamp(c.expires_at, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default();
    let _ = writeln!(
        output,
        "Account: {}\nChatGPT plan usage: {grant}\nAccess token expires: {expires}",
        visible(&c.email)
    );
    print_models(&c.access_token, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_does_not_wait_for_another_refresh() {
        let _guard = TOKEN_LOCK.lock().unwrap();
        let token = Arc::new(AtomicBool::new(true));
        assert_eq!(
            token_cancelable(false, "", Some(token)).unwrap_err(),
            "refresh ChatGPT credentials: operation cancelled"
        );
    }

    #[test]
    fn credential_filesystem_errors_use_owned_categories() {
        for (code, expected) in [
            (libc::ELOOP, "too many symbolic links"),
            (libc::ENOENT, "path does not exist"),
            (libc::EACCES, "permission denied"),
        ] {
            assert_eq!(stable(std::io::Error::from_raw_os_error(code)), expected);
        }
    }

    #[test]
    fn cache_obeys_refresh_embargo_and_rejected_token_race() {
        let mut c = Credentials {
            client_id: "client".into(),
            access_token: "new".into(),
            refresh_token: "refresh".into(),
            scope: "openid chatgpt.tokens.use.direct".into(),
            expires_at: now() + 60,
            earliest_refresh_at: now() + 120,
            ..Default::default()
        };
        assert_eq!(cached(&c, false, "").unwrap(), Some("new".into()));
        assert!(cached(&c, true, "new").is_err());
        assert_eq!(cached(&c, true, "old").unwrap(), Some("new".into()));
        c.earliest_refresh_at = 0;
        assert_eq!(cached(&c, false, "").unwrap(), None);
    }

    #[test]
    fn delayed_fragmented_http_callback_completes_login() {
        if std::env::var_os("REAGENT_TEST_FRAGMENTED_CALLBACK").is_none() {
            use std::os::unix::fs::PermissionsExt;
            let directory = tempfile::tempdir().unwrap();
            let browser = directory.path().join(if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            });
            std::fs::write(&browser, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "auth::tests::delayed_fragmented_http_callback_completes_login",
                    "--nocapture",
                ])
                .env("REAGENT_TEST_FRAGMENTED_CALLBACK", "1")
                .env("PATH", directory.path())
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            // Leave time for accept before sending any bytes, then split both request line and headers.
            for part in [
                "GET /auth/callback?state=s&code=",
                "c&client_id=oaiapp_test HTTP/1.1\r\nHost: ",
                &format!("{address}\r\n\r\n"),
            ] {
                std::thread::sleep(Duration::from_millis(100));
                stream.write_all(part.as_bytes()).unwrap();
            }
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        });
        let result = wait_callback(
            &listener,
            &format!("http://{address}/auth/callback"),
            "s",
            "oaiapp_test",
            "https://auth.example.invalid/authorize",
            &mut Vec::new(),
            Instant::now() + Duration::from_secs(3),
        );
        let response = client.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "{response}");
        let values = result.unwrap();
        assert_eq!(values["code"], "c");
        assert_eq!(values["client_id"], "oaiapp_test");
    }

    #[test]
    fn callback_rejects_duplicate_state_and_foreign_client() {
        let redirect = "http://127.0.0.1:1455/auth/callback";
        assert!(callback(
            &format!("{redirect}?state=s&state=s&code=c&client_id=oaiapp_new"),
            redirect,
            "s",
            ""
        )
        .is_err());
        assert!(callback(
            &format!("{redirect}?state=s&code=c&client_id=oaiapp_other"),
            redirect,
            "s",
            "oaiapp_saved"
        )
        .is_err());
        assert!(callback(
            &format!("{redirect}?state=s&code=c&client_id=oaiapp_new"),
            redirect,
            "s",
            ""
        )
        .is_ok());
    }

    #[test]
    fn id_token_binds_nonce_audience_subject_and_expiry() {
        let claims = json!({"iss":"https://issuer","aud":["client","other"],"exp":now()+3600,"nonce":"n","sub":"s","email":"s@example.com"});
        let token = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(json_string(&claims))
        );
        assert!(validate_id_token(&token, "https://issuer", "client", "n", "s").is_ok());
        assert!(validate_id_token(&token, "https://issuer", "client", "wrong", "s").is_err());
        assert!(validate_id_token(&token, "https://issuer", "wrong", "n", "s").is_err());
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    #[test]
    fn authorization_form_sends_no_accept_header() {
        let (url, server) = crate::transport::http::tests::no_accept_server(
            br#"{"access_token":"token"}"#.to_vec(),
        );
        let result = form(
            &client(Duration::from_secs(2)).unwrap(),
            &url,
            &[("grant_type", "refresh_token"), ("refresh_token", "value")],
            true,
        );
        assert_eq!(
            result.map_err(|e| e.to_string()).unwrap()["access_token"],
            "token"
        );
        server.join().unwrap();
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn credential_wait_honors_request_deadline_without_cancelling_ordinary_policy() {
        let guard = TOKEN_LOCK.lock().unwrap();
        let thread = std::thread::spawn(|| {
            let started = Instant::now();
            let error = token_until(false, "", None, Some(started + Duration::from_millis(40)))
                .unwrap_err();
            assert_eq!(error, "refresh ChatGPT credentials: operation timed out");
            assert!(started.elapsed() < Duration::from_millis(250));
        });
        thread.join().unwrap();
        drop(guard);
    }
}
