//! Who may talk to the daemon (PLAN.md section 9).
//!
//! - Every request must name the daemon by its loopback address in `Host` (stops DNS
//!   rebinding) and, from a browser, come from the daemon's own page (`Origin`).
//! - The CLI sends the token from the user-only `token` file as `Authorization: Bearer`.
//! - A browser gets a one-time code from `assist ui` in the page URL fragment, swaps it once
//!   for an `HttpOnly`, `SameSite=Strict` session cookie, and uses that.
//! - After [`MAX_FAILURES`] failed attempts in a minute, further failures are answered with
//!   `429 Too Many Requests`. Valid credentials always pass, so the owner is never locked out.

use assist_core::secrets::constant_time_eq;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use assist_core::private_fs;
use assist_proto::now_ms;
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};
use axum::http::HeaderMap;

/// Session cookie name.
pub const COOKIE_NAME: &str = "assist_session";
/// A one-time code is valid this long.
pub const OTP_TTL_MS: u64 = 60_000;
/// Failed attempts per minute before failures get `429`.
pub const MAX_FAILURES: u32 = 10;

const TOKEN_BYTES: usize = 32;
const FAILURE_WINDOW_MS: u64 = 60_000;

/// How a connection proved who it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// The token from the token file (CLI).
    Token,
    /// A browser session cookie.
    Session(String),
    /// A paired device's key, checked by iroh; the device's name.
    Node(String),
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No or wrong credential.
    Unauthorized,
    /// Too many failures recently.
    TooMany,
}

/// Credentials and sessions of one daemon run.
pub struct Auth {
    token: String,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Code -> expiry, Unix ms.
    otps: HashMap<String, u64>,
    sessions: HashMap<String, u64>,
    failures: u32,
    window_start: u64,
}

impl Auth {
    /// Uses the token in `dir/token`, creating it (owner-only) on first run.
    pub fn open(dir: &Path) -> io::Result<Self> {
        Ok(Self {
            token: load_or_create_token(dir)?,
            state: Mutex::new(State::default()),
        })
    }

    /// Checks the bearer token or session cookie in `headers`.
    pub fn check(&self, headers: &HeaderMap) -> Result<Credential, Refusal> {
        let bearer = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if bearer.is_some_and(|given| constant_time_eq(given.as_bytes(), self.token.as_bytes())) {
            return Ok(Credential::Token);
        }
        if let Some(session) = session_cookie(headers) {
            if self.lock().sessions.contains_key(session) {
                return Ok(Credential::Session(session.to_owned()));
            }
        }
        Err(self.failed())
    }

    /// A new one-time code for [`Auth::redeem`].
    pub fn new_otp(&self) -> io::Result<String> {
        let code = random_hex(16)?;
        let mut state = self.lock();
        let now = now_ms();
        state.otps.retain(|_, expiry| *expiry > now);
        state.otps.insert(code.clone(), now + OTP_TTL_MS);
        Ok(code)
    }

    /// Swaps a one-time code for a new session id. Each code works once.
    pub fn redeem(&self, code: &str) -> Result<String, Refusal> {
        let valid = {
            let mut state = self.lock();
            state
                .otps
                .remove(code)
                .is_some_and(|expiry| expiry > now_ms())
        };
        if !valid {
            return Err(self.failed());
        }
        let session = random_hex(TOKEN_BYTES).map_err(|_| Refusal::Unauthorized)?;
        self.lock().sessions.insert(session.clone(), now_ms());
        Ok(session)
    }

    /// Ends a browser session.
    pub fn end_session(&self, session: &str) {
        self.lock().sessions.remove(session);
    }

    fn failed(&self) -> Refusal {
        let mut state = self.lock();
        let now = now_ms();
        if now.saturating_sub(state.window_start) > FAILURE_WINDOW_MS {
            state.window_start = now;
            state.failures = 0;
        }
        state.failures = state.failures.saturating_add(1);
        if state.failures > MAX_FAILURES {
            Refusal::TooMany
        } else {
            Refusal::Unauthorized
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether `Host` is this daemon's loopback address and `port`.
pub fn host_ok(headers: &HeaderMap, port: u16) -> bool {
    headers
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| allowed_hosts(port).iter().any(|ok| ok == host))
}

/// Whether `Origin` is absent (non-browser client) or this daemon's own page.
pub fn origin_ok(headers: &HeaderMap, port: u16) -> bool {
    match headers.get(ORIGIN).map(|v| v.to_str()) {
        None => true,
        Some(Ok(origin)) => allowed_hosts(port)
            .iter()
            .any(|host| origin == format!("http://{host}")),
        Some(Err(_)) => false,
    }
}

fn allowed_hosts(port: u16) -> [String; 3] {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE_NAME).then_some(value)
        })
}

/// `bytes` random bytes from the OS, as lowercase hex.
pub fn random_hex(bytes: usize) -> io::Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|err| io::Error::other(err.to_string()))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The token in `dir/token`, created owner-only if missing or malformed.
pub fn load_or_create_token(dir: &Path) -> io::Result<String> {
    private_fs::create_dir(dir)?;
    let path = dir.join("token");
    let mut existing = String::new();
    if let Ok(mut file) = std::fs::File::open(&path) {
        file.read_to_string(&mut existing)?;
    }
    let existing = existing.trim();
    if existing.len() == TOKEN_BYTES * 2 && existing.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(existing.to_owned());
    }
    let token = random_hex(TOKEN_BYTES)?;
    let _ = std::fs::remove_file(&path);
    let mut file = private_fs::open_append(&path)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(axum::http::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn auth() -> (Auth, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "assist-auth-{}-{}",
            std::process::id(),
            random_hex(4).unwrap()
        ));
        (Auth::open(&dir).unwrap(), dir)
    }

    #[test]
    fn token_is_created_once_and_reused() {
        let (auth, dir) = auth();
        assert_eq!(auth.token.len(), 64);
        assert_eq!(load_or_create_token(&dir).unwrap(), auth.token);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bearer_otp_and_session_flow() {
        let (auth, dir) = auth();
        let bearer = format!("Bearer {}", auth.token);
        assert_eq!(
            auth.check(&headers(&[(AUTHORIZATION, &bearer)])),
            Ok(Credential::Token)
        );
        assert_eq!(
            auth.check(&headers(&[(AUTHORIZATION, "Bearer wrong")])),
            Err(Refusal::Unauthorized)
        );

        let code = auth.new_otp().unwrap();
        let session = auth.redeem(&code).unwrap();
        assert!(auth.redeem(&code).is_err(), "codes work once");
        let cookie = format!("theme=dark; {COOKIE_NAME}={session}");
        assert_eq!(
            auth.check(&headers(&[(COOKIE, &cookie)])),
            Ok(Credential::Session(session.clone()))
        );
        auth.end_session(&session);
        assert!(auth.check(&headers(&[(COOKIE, &cookie)])).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn repeated_failures_are_throttled_but_the_owner_still_gets_in() {
        let (auth, dir) = auth();
        let wrong = headers(&[(AUTHORIZATION, "Bearer nope")]);
        for _ in 0..MAX_FAILURES {
            assert_eq!(auth.check(&wrong), Err(Refusal::Unauthorized));
        }
        assert_eq!(auth.check(&wrong), Err(Refusal::TooMany));
        let bearer = format!("Bearer {}", auth.token);
        assert!(auth.check(&headers(&[(AUTHORIZATION, &bearer)])).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn host_and_origin_must_be_the_daemon() {
        assert!(host_ok(&headers(&[(HOST, "127.0.0.1:7720")]), 7720));
        assert!(host_ok(&headers(&[(HOST, "localhost:7720")]), 7720));
        assert!(!host_ok(&headers(&[(HOST, "evil.example:7720")]), 7720));
        assert!(!host_ok(&headers(&[(HOST, "127.0.0.1:9999")]), 7720));
        assert!(!host_ok(&HeaderMap::new(), 7720));
        assert!(origin_ok(&HeaderMap::new(), 7720));
        assert!(origin_ok(
            &headers(&[(ORIGIN, "http://127.0.0.1:7720")]),
            7720
        ));
        assert!(!origin_ok(
            &headers(&[(ORIGIN, "https://evil.example")]),
            7720
        ));
        assert!(!origin_ok(&headers(&[(ORIGIN, "null")]), 7720));
    }
}
