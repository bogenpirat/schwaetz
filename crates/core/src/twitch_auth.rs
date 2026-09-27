//! "Sign in with Twitch": the OAuth device code flow and a token manager that keeps the user's
//! access token usable for the Helix API.
//!
//! Twitch only offers the device code flow to *public* clients (desktop apps that cannot keep a
//! client secret). The user opens a Twitch page with the code already filled in and clicks
//! Authorize, while we poll for the tokens. Public-client refresh tokens are one-time use and
//! expire 30 days after they are issued, so [`TokenManager`] refreshes at least every 24 hours
//! (sooner if the access token expires first, or right away after a 401) and stores every new
//! refresh token immediately.
//!
//! Like the rest of the model this is sans-IO: [`TokenManager`] decides what to do and returns
//! [`AuthRequest`]s; [`execute`] performs one (blocking HTTPS, run it on a worker thread) and the
//! [`AuthResponse`] goes back into [`TokenManager::on_response`].

use serde::{Deserialize, Serialize};

/// The Twitch application compiled into this build (`SCHWAETZ_TWITCH_CLIENT_ID`, set in
/// `.cargo/config.toml`), if any.
pub fn built_in_client_id() -> Option<&'static str> {
    option_env!("SCHWAETZ_TWITCH_CLIENT_ID").map(str::trim).filter(|s| !s.is_empty())
}

/// Scopes requested at sign-in: live checks need none; emote completion needs the list of
/// emotes the user may use.
pub const SCOPES: &str = crate::helix::EMOTES_SCOPE;

/// Refresh at least this often: a public client's refresh token dies 30 days after it was issued,
/// and refreshing replaces it.
pub const MAX_TOKEN_AGE_MS: i64 = 24 * 60 * 60 * 1000;
/// Refresh this long before the access token's own expiry.
const EXPIRY_MARGIN_MS: i64 = 10 * 60 * 1000;
/// Wait before retrying after a network or server error.
const RETRY_MS: i64 = 5 * 60 * 1000;

/// What is stored (as JSON in the Credential Manager) after signing in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// When the access token was issued (Unix ms).
    pub obtained_at: i64,
    /// Access token lifetime in seconds, as reported by Twitch.
    pub expires_in: u64,
    /// Twitch login of the signed-in user.
    #[serde(default)]
    pub login: String,
}

impl Tokens {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Option<Tokens> {
        serde_json::from_str(s).ok().filter(|t: &Tokens| !t.refresh.is_empty())
    }

    /// When the next refresh is due (Unix ms).
    pub fn refresh_due(&self) -> i64 {
        let life = (self.expires_in as i64 * 1000 - EXPIRY_MARGIN_MS).max(60_000);
        self.obtained_at + life.min(MAX_TOKEN_AGE_MS)
    }
}

/// One HTTPS exchange with Twitch's OAuth server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthRequest {
    StartDevice { client_id: String },
    PollDevice { client_id: String, device_code: String },
    Refresh { client_id: String, refresh_token: String },
    Revoke { client_id: String, token: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthResponse {
    Device {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_in: u64,
        interval: u64,
    },
    /// The user has not authorized yet.
    Pending,
    /// Poll less often.
    SlowDown,
    Tokens {
        access: String,
        refresh: String,
        expires_in: u64,
        login: String,
    },
    /// The grant is gone (denied, expired device code, revoked or used refresh token): the user
    /// has to sign in again.
    Invalid(String),
    /// A temporary problem (network, Twitch unavailable): try again later.
    Failed(String),
    Revoked,
}

/// What the app has to do after a token manager step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthEvent {
    /// Store these tokens (or delete the stored ones for `None`).
    Persist(Option<Tokens>),
    /// Show the user the Twitch page where they authorize (the code is already filled in).
    OpenBrowser {
        url: String,
        user_code: String,
    },
    SignedIn {
        login: String,
    },
    /// Signing in stopped without tokens (denied, expired, cancelled).
    SignInFailed(String),
    /// Stored tokens stopped working; the user has to sign in again.
    SignedOut(String),
    /// A temporary problem worth mentioning once.
    Problem(String),
}

#[derive(Clone, Debug)]
struct DeviceFlow {
    device_code: String,
    user_code: String,
    url: String,
    expires_at: i64,
    interval_ms: i64,
    next_poll: i64,
}

/// Owns one account's tokens and decides when to talk to Twitch.
#[derive(Clone, Debug, Default)]
pub struct TokenManager {
    client_id: Option<String>,
    tokens: Option<Tokens>,
    /// Waiting for Twitch to hand out a device code.
    starting: bool,
    device: Option<DeviceFlow>,
    in_flight: bool,
    /// Refresh at this time instead of the regular schedule (after a 401 or an error).
    refresh_at: Option<i64>,
}

impl TokenManager {
    pub fn new(client_id: Option<String>, tokens: Option<Tokens>) -> TokenManager {
        TokenManager { client_id: client_id.filter(|c| !c.is_empty()), tokens, ..Default::default() }
    }

    /// Whether this build (or the network's settings) has a Twitch application to sign in with.
    pub fn available(&self) -> bool {
        self.client_id.is_some()
    }

    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }

    pub fn access_token(&self) -> Option<&str> {
        self.tokens.as_ref().map(|t| t.access.as_str()).filter(|a| !a.is_empty())
    }

    /// The signed-in login ("" if Twitch did not tell us).
    pub fn login(&self) -> Option<&str> {
        self.tokens.as_ref().map(|t| t.login.as_str())
    }

    pub fn signing_in(&self) -> bool {
        self.starting || self.device.is_some()
    }

    /// The code shown on the Twitch page while signing in.
    pub fn user_code(&self) -> Option<&str> {
        self.device.as_ref().map(|d| d.user_code.as_str())
    }

    /// The Twitch page to open while signing in.
    pub fn verification_url(&self) -> Option<&str> {
        self.device.as_ref().map(|d| d.url.as_str())
    }

    /// Starts the device code flow.
    pub fn sign_in(&mut self) -> Option<AuthRequest> {
        let client_id = self.client_id.clone()?;
        if self.signing_in() {
            return None;
        }
        self.starting = true;
        self.in_flight = true;
        Some(AuthRequest::StartDevice { client_id })
    }

    /// Stops a sign-in in progress.
    pub fn cancel(&mut self) {
        self.starting = false;
        self.device = None;
    }

    /// Forgets the tokens; returns the revocation to send (best effort) and what to persist.
    pub fn sign_out(&mut self) -> (Option<AuthRequest>, Vec<AuthEvent>) {
        self.cancel();
        self.refresh_at = None;
        let Some(t) = self.tokens.take() else { return (None, Vec::new()) };
        let revoke = self.client_id.clone().map(|client_id| AuthRequest::Revoke { client_id, token: t.access });
        (revoke, vec![AuthEvent::Persist(None)])
    }

    /// The API rejected the access token: refresh as soon as possible.
    pub fn unauthorized(&mut self, now: i64) {
        if self.tokens.is_some() {
            self.refresh_at = Some(now);
        }
    }

    /// Periodic step: poll a pending sign-in or refresh when due.
    pub fn poll(&mut self, now: i64) -> Option<AuthRequest> {
        if self.in_flight {
            return None;
        }
        let client_id = self.client_id.clone()?;
        if let Some(d) = &mut self.device {
            if now < d.next_poll {
                return None;
            }
            d.next_poll = now + d.interval_ms;
            self.in_flight = true;
            return Some(AuthRequest::PollDevice { client_id, device_code: d.device_code.clone() });
        }
        let t = self.tokens.as_ref()?;
        let due = self.refresh_at.unwrap_or_else(|| t.refresh_due());
        if now < due {
            return None;
        }
        self.in_flight = true;
        Some(AuthRequest::Refresh { client_id, refresh_token: t.refresh.clone() })
    }

    /// Takes the result of an [`AuthRequest`] this manager produced.
    pub fn on_response(&mut self, req: &AuthRequest, resp: AuthResponse, now: i64) -> Vec<AuthEvent> {
        if !matches!(req, AuthRequest::Revoke { .. }) {
            self.in_flight = false;
        }
        let mut ev = Vec::new();
        match (req, resp) {
            (AuthRequest::Revoke { .. }, _) => {}
            (
                AuthRequest::StartDevice { .. },
                AuthResponse::Device { device_code, user_code, verification_uri, expires_in, interval },
            ) => {
                if !self.starting {
                    return ev; // cancelled meanwhile
                }
                self.starting = false;
                let interval_ms = interval.max(1) as i64 * 1000;
                ev.push(AuthEvent::OpenBrowser { url: verification_uri.clone(), user_code: user_code.clone() });
                self.device = Some(DeviceFlow {
                    device_code,
                    user_code,
                    url: verification_uri,
                    expires_at: now + expires_in as i64 * 1000,
                    interval_ms,
                    next_poll: now + interval_ms,
                });
            }
            (AuthRequest::StartDevice { .. }, AuthResponse::Invalid(e) | AuthResponse::Failed(e)) => {
                self.starting = false;
                ev.push(AuthEvent::SignInFailed(e));
            }
            (AuthRequest::PollDevice { .. }, AuthResponse::Pending) => self.check_expiry(now, &mut ev),
            (AuthRequest::PollDevice { .. }, AuthResponse::SlowDown) => {
                if let Some(d) = &mut self.device {
                    d.interval_ms += 5000;
                    d.next_poll = now + d.interval_ms;
                }
                self.check_expiry(now, &mut ev);
            }
            (AuthRequest::PollDevice { .. }, AuthResponse::Failed(_)) => self.check_expiry(now, &mut ev),
            (AuthRequest::PollDevice { .. }, AuthResponse::Invalid(e)) => {
                if self.device.take().is_some() {
                    ev.push(AuthEvent::SignInFailed(e));
                }
            }
            (
                AuthRequest::PollDevice { .. } | AuthRequest::Refresh { .. },
                AuthResponse::Tokens { access, refresh, expires_in, login },
            ) => {
                let signing_in = matches!(req, AuthRequest::PollDevice { .. });
                if signing_in && self.device.take().is_none() {
                    return ev; // cancelled meanwhile
                }
                let login = if login.is_empty() { self.login().unwrap_or_default().to_owned() } else { login };
                let t = Tokens { access, refresh, obtained_at: now, expires_in, login: login.clone() };
                self.tokens = Some(t.clone());
                self.refresh_at = None;
                ev.push(AuthEvent::Persist(Some(t)));
                if signing_in {
                    ev.push(AuthEvent::SignedIn { login });
                }
            }
            (AuthRequest::Refresh { .. }, AuthResponse::Invalid(e)) => {
                if self.tokens.take().is_some() {
                    self.refresh_at = None;
                    ev.push(AuthEvent::Persist(None));
                    ev.push(AuthEvent::SignedOut(e));
                }
            }
            (AuthRequest::Refresh { .. }, AuthResponse::Failed(e)) if self.tokens.is_some() => {
                self.refresh_at = Some(now + RETRY_MS);
                ev.push(AuthEvent::Problem(e));
            }
            _ => {}
        }
        ev
    }

    fn check_expiry(&mut self, now: i64, ev: &mut Vec<AuthEvent>) {
        if self.device.as_ref().is_some_and(|d| now >= d.expires_at) {
            self.device = None;
            ev.push(AuthEvent::SignInFailed("the sign-in code expired".into()));
        }
    }
}

// ----- HTTPS -------------------------------------------------------------------------------------

const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const MAX_BODY: u64 = 64 * 1024;

/// Performs one request (blocking).
pub fn execute(req: &AuthRequest) -> AuthResponse {
    match req {
        AuthRequest::StartDevice { client_id } => {
            let fields = [("client_id", client_id.as_str()), ("scopes", SCOPES)];
            match post("https://id.twitch.tv/oauth2/device", &fields) {
                Ok((200, body)) => parse_device(&body).unwrap_or_else(AuthResponse::Failed),
                Ok((status, body)) => classify_error(status, &body),
                Err(e) => AuthResponse::Failed(e),
            }
        }
        AuthRequest::PollDevice { client_id, device_code } => {
            let fields = [
                ("client_id", client_id.as_str()),
                ("scopes", SCOPES),
                ("device_code", device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ];
            token_response(post(TOKEN_URL, &fields))
        }
        AuthRequest::Refresh { client_id, refresh_token } => {
            let fields = [
                ("client_id", client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.as_str()),
            ];
            token_response(post(TOKEN_URL, &fields))
        }
        AuthRequest::Revoke { client_id, token } => {
            let fields = [("client_id", client_id.as_str()), ("token", token.as_str())];
            let _ = post("https://id.twitch.tv/oauth2/revoke", &fields);
            AuthResponse::Revoked
        }
    }
}

fn post(url: &str, fields: &[(&str, &str)]) -> Result<(u16, Vec<u8>), String> {
    schwaetz_net::http::post_form(url, fields, MAX_BODY).map(|r| (r.status, r.body))
}

fn token_response(r: Result<(u16, Vec<u8>), String>) -> AuthResponse {
    match r {
        Ok((200, body)) => match parse_tokens(&body) {
            Ok((access, refresh, expires_in)) => {
                let login = validate_login(&access).unwrap_or_default();
                AuthResponse::Tokens { access, refresh, expires_in, login }
            }
            Err(e) => AuthResponse::Failed(e),
        },
        Ok((status, body)) => classify_error(status, &body),
        Err(e) => AuthResponse::Failed(e),
    }
}

/// Learns the login the token belongs to.
fn validate_login(access: &str) -> Option<String> {
    let auth = format!("OAuth {access}");
    let r = schwaetz_net::http::get_with(
        "https://id.twitch.tv/oauth2/validate",
        &[("Authorization", &auth)],
        MAX_BODY,
        false,
    )
    .ok()?;
    #[derive(Deserialize)]
    struct V {
        #[serde(default)]
        login: String,
    }
    (r.status == 200).then(|| serde_json::from_slice::<V>(&r.body).ok().map(|v| v.login)).flatten()
}

pub fn parse_device(body: &[u8]) -> Result<AuthResponse, String> {
    #[derive(Deserialize)]
    struct D {
        device_code: String,
        user_code: String,
        verification_uri: String,
        #[serde(default = "default_expiry")]
        expires_in: u64,
        #[serde(default = "default_interval")]
        interval: u64,
    }
    fn default_expiry() -> u64 {
        1800
    }
    fn default_interval() -> u64 {
        5
    }
    let d: D = serde_json::from_slice(body).map_err(|e| format!("unexpected response from Twitch: {e}"))?;
    Ok(AuthResponse::Device {
        device_code: d.device_code,
        user_code: d.user_code,
        verification_uri: d.verification_uri,
        expires_in: d.expires_in,
        interval: d.interval,
    })
}

pub fn parse_tokens(body: &[u8]) -> Result<(String, String, u64), String> {
    #[derive(Deserialize)]
    struct T {
        access_token: String,
        refresh_token: String,
        #[serde(default)]
        expires_in: u64,
    }
    let t: T = serde_json::from_slice(body).map_err(|e| format!("unexpected response from Twitch: {e}"))?;
    Ok((t.access_token, t.refresh_token, t.expires_in))
}

/// Maps an OAuth error (`{"status":400,"message":"authorization_pending"}`) to a response.
pub fn classify_error(status: u16, body: &[u8]) -> AuthResponse {
    #[derive(Deserialize)]
    struct E {
        #[serde(default)]
        message: String,
    }
    let msg = serde_json::from_slice::<E>(body).map(|e| e.message).unwrap_or_default();
    match msg.as_str() {
        "authorization_pending" => AuthResponse::Pending,
        "slow_down" => AuthResponse::SlowDown,
        _ if status >= 500 || status == 429 => AuthResponse::Failed(format!("Twitch is unavailable (HTTP {status})")),
        "access_denied" => AuthResponse::Invalid("access was denied".into()),
        "invalid device code" | "expired_token" => AuthResponse::Invalid("the sign-in code expired".into()),
        "Invalid refresh token" => AuthResponse::Invalid("the sign-in expired or was revoked".into()),
        "Invalid client credentials" | "invalid client" => {
            AuthResponse::Invalid("Twitch rejected this application (it must be registered as a Public client)".into())
        }
        "" => AuthResponse::Invalid(format!("Twitch refused the request (HTTP {status})")),
        m => AuthResponse::Invalid(m.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000;

    fn tokens(m: &mut TokenManager, req: &AuthRequest, now: i64, n: u32) -> Vec<AuthEvent> {
        let resp = AuthResponse::Tokens {
            access: format!("a{n}"),
            refresh: format!("r{n}"),
            expires_in: 14_400,
            login: if n == 1 { "alice".into() } else { String::new() },
        };
        m.on_response(req, resp, now)
    }

    #[test]
    fn device_flow_then_scheduled_refresh() {
        let mut m = TokenManager::new(Some("cid".into()), None);
        assert!(m.poll(T0).is_none(), "nothing to do before signing in");
        let start = m.sign_in().unwrap();
        assert_eq!(start, AuthRequest::StartDevice { client_id: "cid".into() });
        assert!(m.sign_in().is_none(), "one sign-in at a time");

        let device = AuthResponse::Device {
            device_code: "dc".into(),
            user_code: "ABCDEFGH".into(),
            verification_uri: "https://www.twitch.tv/activate?public=true&device-code=ABCDEFGH".into(),
            expires_in: 1800,
            interval: 5,
        };
        let ev = m.on_response(&start, device, T0);
        assert!(matches!(&ev[..], [AuthEvent::OpenBrowser { user_code, .. }] if user_code == "ABCDEFGH"));
        assert!(m.poll(T0 + 1000).is_none(), "respects the polling interval");
        let poll = m.poll(T0 + 5000).unwrap();
        assert!(matches!(&poll, AuthRequest::PollDevice { device_code, .. } if device_code == "dc"));
        assert!(m.poll(T0 + 20_000).is_none(), "one request in flight");
        assert!(m.on_response(&poll, AuthResponse::Pending, T0 + 5100).is_empty());
        let poll = m.poll(T0 + 10_100).unwrap();
        let ev = tokens(&mut m, &poll, T0 + 10_200, 1);
        assert_eq!(ev[1], AuthEvent::SignedIn { login: "alice".into() });
        assert!(matches!(&ev[0], AuthEvent::Persist(Some(t)) if t.refresh == "r1"));
        assert_eq!(m.access_token(), Some("a1"));
        assert!(!m.signing_in());

        // Access tokens living 4 h are refreshed 10 min before they expire.
        let due = T0 + 10_200 + 14_400_000 - 600_000;
        assert!(m.poll(due - 1).is_none());
        let refresh = m.poll(due).unwrap();
        assert_eq!(refresh, AuthRequest::Refresh { client_id: "cid".into(), refresh_token: "r1".into() });
        let ev = tokens(&mut m, &refresh, due, 2);
        assert!(matches!(&ev[..], [AuthEvent::Persist(Some(t))] if t.refresh == "r2" && t.login == "alice"));
    }

    #[test]
    fn refreshes_daily_and_on_unauthorized() {
        let t = Tokens {
            access: "a".into(),
            refresh: "r".into(),
            obtained_at: T0,
            expires_in: 5_000_000,
            login: "bob".into(),
        };
        assert_eq!(t.refresh_due(), T0 + MAX_TOKEN_AGE_MS, "long-lived tokens still refresh every 24 h");
        let mut m = TokenManager::new(Some("cid".into()), Some(t));
        assert!(m.poll(T0 + 1000).is_none());
        m.unauthorized(T0 + 2000);
        let req = m.poll(T0 + 2000).unwrap();

        // A temporary failure retries later; an invalid refresh token signs out.
        let ev = m.on_response(&req, AuthResponse::Failed("offline".into()), T0 + 3000);
        assert_eq!(ev, [AuthEvent::Problem("offline".into())]);
        assert!(m.poll(T0 + 4000).is_none());
        let req = m.poll(T0 + 3000 + RETRY_MS).unwrap();
        let ev = m.on_response(&req, AuthResponse::Invalid("gone".into()), T0 + RETRY_MS + 4000);
        assert_eq!(ev, [AuthEvent::Persist(None), AuthEvent::SignedOut("gone".into())]);
        assert_eq!(m.access_token(), None);
    }

    #[test]
    fn cancel_sign_out_and_expiry() {
        let mut m = TokenManager::new(Some("cid".into()), None);
        let start = m.sign_in().unwrap();
        m.cancel();
        let device = AuthResponse::Device {
            device_code: "dc".into(),
            user_code: "X".into(),
            verification_uri: "u".into(),
            expires_in: 10,
            interval: 5,
        };
        assert!(m.on_response(&start, device.clone(), T0).is_empty(), "cancelled before the code came back");

        let start = m.sign_in().unwrap();
        m.on_response(&start, device, T0);
        let poll = m.poll(T0 + 5000).unwrap();
        m.on_response(&poll, AuthResponse::SlowDown, T0 + 5000);
        assert!(m.poll(T0 + 10_000).is_none(), "slow_down adds 5 s");
        let poll = m.poll(T0 + 15_000).unwrap();
        let ev = m.on_response(&poll, AuthResponse::Pending, T0 + 15_000);
        assert_eq!(ev, [AuthEvent::SignInFailed("the sign-in code expired".into())]);

        let t =
            Tokens { access: "a".into(), refresh: "r".into(), obtained_at: T0, expires_in: 100, login: String::new() };
        let mut m = TokenManager::new(Some("cid".into()), Some(t));
        let (revoke, ev) = m.sign_out();
        assert_eq!(revoke, Some(AuthRequest::Revoke { client_id: "cid".into(), token: "a".into() }));
        assert_eq!(ev, [AuthEvent::Persist(None)]);
        assert!(TokenManager::new(None, None).sign_in().is_none(), "no client id, no sign-in");
    }

    #[test]
    fn parses_twitch_responses() {
        let d = br#"{"device_code":"dc","expires_in":1800,"interval":5,"user_code":"HBWSKTCY","verification_uri":"https://www.twitch.tv/activate?device-code=HBWSKTCY"}"#;
        assert!(matches!(parse_device(d).unwrap(), AuthResponse::Device { user_code, .. } if user_code == "HBWSKTCY"));
        let t = br#"{"access_token":"at","expires_in":14124,"refresh_token":"rt","scope":[],"token_type":"bearer"}"#;
        assert_eq!(parse_tokens(t).unwrap(), ("at".into(), "rt".into(), 14124));
        assert_eq!(classify_error(400, br#"{"status":400,"message":"authorization_pending"}"#), AuthResponse::Pending);
        assert!(matches!(
            classify_error(400, br#"{"status":400,"message":"Invalid refresh token"}"#),
            AuthResponse::Invalid(_)
        ));
        assert!(matches!(classify_error(503, b"oops"), AuthResponse::Failed(_)));
        let t = Tokens { access: "a".into(), refresh: "r".into(), obtained_at: 5, expires_in: 60, login: "x".into() };
        assert_eq!(Tokens::from_json(&t.to_json()), Some(t));
        assert_eq!(Tokens::from_json("{}"), None);
    }
}
