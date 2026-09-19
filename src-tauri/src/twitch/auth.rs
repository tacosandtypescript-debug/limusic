//! Twitch OAuth: the Device Code Grant Flow (DCF) with a **public** client.
//!
//! Why not the Authorization Code Grant: it is documented as "meant for apps that use a server,
//! can securely store a client secret, and can make server-to-server requests", and Twitch's own
//! rules are unambiguous about the secret — *"Treat client secrets as you would your password…
//! never expose it to users, even in an obscured form"*. A desktop binary cannot keep one.
//!
//! Why not PKCE: Twitch does not implement it. Their OIDC discovery document advertises only
//! `"token_endpoint_auth_methods_supported": ["client_secret_post"]`, and neither
//! `code_challenge` nor `code_verifier` appears anywhere in their authorization docs. Planning
//! around PKCE would be planning around something that does not exist.
//!
//! So: DCF with client type Public. The consequences are real and are surfaced in the UI rather
//! than hidden:
//!
//! * **Public clients may not use any other flow** (no client_credentials, no implicit), which is
//!   fine — DCF is the only one we want.
//! * **The refresh token is single-use.** Every refresh returns a new one and invalidates the old,
//!   so failing to persist the replacement logs the user out. [`TokenSet::merge_refresh`] exists
//!   for exactly that.
//! * **The refresh token expires after 30 days of not being used**, for a Public client. Past that
//!   the user has to run the device flow again. That is a documented limit, not a bug here.
//! * **Access tokens last ~4 hours**, but Twitch's own guidance is to refresh *reactively*, on a
//!   401, rather than by watching `expires_in`: *"twitch recommends that apps reactively respond
//!   to HTTP status code 401"*. The expiry is still recorded so the UI can show it and so an idle
//!   app can refresh before it acts.
//!
//! Nothing in this file knows about Tauri, SQLite or the player: it takes a `reqwest::Client` and
//! returns data, which is what lets the module be unit-tested and smoke-tested on its own.

use std::time::Duration;

use serde::{Deserialize, Serialize};

const DEVICE_URL: &str = "https://id.twitch.tv/oauth2/device";
const TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const VALIDATE_URL: &str = "https://id.twitch.tv/oauth2/validate";
const REVOKE_URL: &str = "https://id.twitch.tv/oauth2/revoke";

/// The device-code grant type, spelled out in full — it is a URN, not a short name.
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Everything the integration needs, and nothing more.
///
/// Twitch suspends apps that over-ask: *"If you request more scopes than is required to support
/// your app's functionality, Twitch may suspend your application's access to the Twitch API."*
/// So each scope here is tied to a feature that actually exists in the plan:
///
/// | scope | for |
/// |---|---|
/// | `user:read:chat` | reading `!song`, `!play`, … via `channel.chat.message` (phase 3) |
/// | `user:write:chat` | answering in chat, via `POST /helix/chat/messages` (phase 3) |
/// | `channel:read:redemptions` | reading Channel Points redemptions (phase 4) |
/// | `channel:manage:redemptions` | creating *our own* rewards + fulfilling them (phase 4) |
/// | `bits:read` | `channel.bits.use` / `channel.cheer` (phase 4) |
/// | `channel:read:subscriptions` | subs and gift subs (phase 5) |
/// | `moderator:read:followers` | `channel.follow` v2 — v1 was removed in 2023 (phase 5) |
/// | `channel:read:hype_train` | hype train v2 (phase 5) |
///
/// `channel:read:redemptions` and `channel:manage:redemptions` are both listed on purpose: the
/// first is enough to *read* rewards and redemptions, the second is required to create rewards and
/// to change a redemption's status. See `api.rs` for why we create our own rewards instead of
/// borrowing the streamer's.
pub const SCOPES: &[&str] = &[
    "user:read:chat",
    "user:write:chat",
    "channel:read:redemptions",
    "channel:manage:redemptions",
    "bits:read",
    "channel:read:subscriptions",
    "moderator:read:followers",
    "channel:read:hype_train",
];

/// How long any single OAuth round trip gets. These are small JSON endpoints; if one takes longer
/// than this the connection is the problem, not Twitch.
const TIMEOUT: Duration = Duration::from_secs(20);

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A granted set of tokens, with the expiry resolved to an absolute time.
///
/// `expires_at` is stored rather than `expires_in` because the token outlives the process that
/// received it: a relative "3600 seconds" is meaningless after a restart, and Twitch does not
/// report a token's remaining life except through `/validate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Unix seconds. `i64` and not `u64` so it matches every other wall-clock value in the app
    /// (`Db::now_secs`, `expires_at` in the stream cache, …).
    #[serde(default)]
    pub expires_at: i64,
}

impl TokenSet {
    /// Whether the access token is past its recorded expiry, with `skew` seconds of margin so a
    /// token cannot expire mid-request.
    pub fn is_expired(&self, now: i64, skew: i64) -> bool {
        self.expires_at <= now + skew
    }

    /// Take the rotated refresh token from a refresh response, keeping the old one if the response
    /// did not carry a new one.
    ///
    /// This is the single-use rule in code: Twitch *"response contains the new access token,
    /// refresh token, and scopes… Because refresh tokens may change, your app should safely store
    /// the new refresh token to use the next time."* Dropping it here would leave the stored token
    /// already invalidated, and the next refresh would fail with a 401 that looks like a revoked
    /// login.
    pub fn merge_refresh(&mut self, fresh: TokenSet) {
        self.access_token = fresh.access_token;
        self.expires_at = fresh.expires_at;
        if !fresh.scopes.is_empty() {
            self.scopes = fresh.scopes;
        }
        if fresh.refresh_token.is_some() {
            self.refresh_token = fresh.refresh_token;
        }
    }
}

/// The first half of the device flow: what to show the user, and what to poll with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    /// The short code the user types on twitch.tv/activate.
    pub user_code: String,
    /// Twitch normally returns `https://www.twitch.tv/activate?public=true&device-code=…`, ready
    /// to open in a browser — do not rebuild it from parts.
    pub verification_uri: String,
    pub expires_in: i64,
    /// Seconds to wait between polls. Twitch's example is 5.
    pub interval: i64,
}

impl DeviceCode {
    /// The polling interval, floored at 1s. A server-supplied 0 would otherwise spin the loop.
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.interval.clamp(1, 60) as u64)
    }
}

/// The answer to a single poll. The *loop* lives in `mod.rs` so that it can be cancelled when the
/// user disconnects or retries, which is why this is one step and not a `while` here.
#[derive(Debug)]
pub enum Poll {
    /// The user has not approved yet — `authorization_pending` (HTTP 400 per the docs).
    Pending,
    /// The server asked us to slow down. Not in Twitch's documented error list, but it is part of
    /// the standard device-flow contract and costs one match arm to honour.
    SlowDown,
    Granted(Box<TokenSet>),
    /// Terminal: the code expired, was already used, or the user denied. Carries the message to
    /// show verbatim — Twitch's own wording is clearer than anything we would paraphrase.
    Failed(String),
}

/// A successful `/validate`, which is the only trustworthy source for "who is this token".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Validated {
    pub client_id: String,
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_in: i64,
}

/// Why a refresh failed. The distinction drives the UI: one is "try again", the other is "you must
/// sign in again", and conflating them produces a connect button that does nothing.
#[derive(Debug)]
pub enum RefreshError {
    /// The refresh token is dead (expired after 30 days, revoked, or password changed). The user
    /// has to run the device flow again.
    Rejected(String),
    /// Network, 5xx, 429 — the stored token may still be perfectly good.
    Transient(String),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::Rejected(m) => write!(f, "{m}"),
            RefreshError::Transient(m) => write!(f, "{m}"),
        }
    }
}

/// Twitch's error envelope: `{"error":"…","status":401,"message":"…"}`. Only `message` is read —
/// the envelope's `status` duplicates the HTTP status, which is what the callers actually branch
/// on, so carrying it further would create two sources for one fact.
#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

/// Best-effort extraction of `message` from an error body, falling back to the raw text (HTML
/// error pages and empty bodies both happen).
fn error_message(status: reqwest::StatusCode, body: &str) -> String {
    match serde_json::from_str::<ApiError>(body) {
        Ok(e) if !e.message.is_empty() => e.message,
        _ => {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                format!("HTTP {}", status.as_u16())
            } else {
                format!(
                    "HTTP {}: {}",
                    status.as_u16(),
                    trimmed.chars().take(200).collect::<String>()
                )
            }
        }
    }
}

/// Start the device flow. Returns what to show the user.
pub async fn start_device_flow(
    http: &reqwest::Client,
    client_id: &str,
) -> Result<DeviceCode, String> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err("No Twitch client ID configured.".into());
    }
    let resp = http
        .post(DEVICE_URL)
        .form(&[("client_id", client_id), ("scopes", &SCOPES.join(" "))])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Could not reach Twitch: {e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // A wrong client ID is the overwhelmingly likely failure here, and Twitch's message for it
        // ("invalid client") does not say which of the two fields to fix. Say it.
        return Err(format!(
            "Twitch refused the device code request: {}",
            error_message(status, &body)
        ));
    }
    serde_json::from_str::<DeviceCode>(&body)
        .map_err(|e| format!("Twitch sent an unexpected device-code response: {e}"))
}

/// One poll of the token endpoint. `client_id` only: a public client must not send a secret.
pub async fn poll_device_flow(http: &reqwest::Client, client_id: &str, device_code: &str) -> Poll {
    let params: [(&str, &str); 4] = [
        ("client_id", client_id),
        ("scopes", &SCOPES.join(" ")),
        ("device_code", device_code),
        ("grant_type", DEVICE_GRANT),
    ];
    let resp = match http.post(TOKEN_URL).form(&params).timeout(TIMEOUT).send().await {
        Ok(r) => r,
        Err(e) => return Poll::Failed(format!("Could not reach Twitch: {e}")),
    };

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if status.is_success() {
        return match parse_token_set(&body) {
            Ok(set) => Poll::Granted(Box::new(set)),
            Err(e) => Poll::Failed(e),
        };
    }

    let message = error_message(status, &body);
    // The documented pending signal is an HTTP 400 whose message is `authorization_pending`. Match
    // on the message rather than the status alone: a 400 for a malformed request is a terminal
    // failure, and treating it as "still waiting" would poll until the code expired.
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("authorization_pending") {
        Poll::Pending
    } else if lowered.contains("slow_down") {
        Poll::SlowDown
    } else {
        Poll::Failed(message)
    }
}

/// Exchange a refresh token for a new access token. See [`TokenSet::merge_refresh`] for the
/// single-use rule this feeds.
pub async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenSet, RefreshError> {
    let resp = http
        .post(TOKEN_URL)
        // No `client_secret`: this is a public client. Twitch's own note on the field is "not
        // required if your application's client type was set to public".
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| RefreshError::Transient(format!("Could not reach Twitch: {e}")))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        return parse_token_set(&body).map_err(RefreshError::Transient);
    }
    let message = error_message(status, &body);
    // 400/401 here means the grant itself is gone. 429/5xx means Twitch is unhappy right now and
    // the stored token is still worth keeping.
    let rejected = matches!(status.as_u16(), 400 | 401 | 403);
    if rejected {
        Err(RefreshError::Rejected(message))
    } else {
        Err(RefreshError::Transient(message))
    }
}

/// `GET /oauth2/validate`. Twitch *requires* this: *"Any third-party app that calls the Twitch
/// APIs and maintains an OAuth session must call the /validate endpoint… when it starts and on an
/// hourly basis thereafter"*, and they audit it — *"Twitch reserves the right to take punitive
/// action, such as revoking the developer's API key or throttling the application's performance."*
///
/// So this is not an optimisation; it is a licence condition, and `mod.rs` runs it on a timer.
pub async fn validate(http: &reqwest::Client, access_token: &str) -> Result<Validated, String> {
    let resp = http
        .get(VALIDATE_URL)
        // The documented form is `OAuth <token>`; `Bearer` is also accepted. `OAuth` matches the
        // docs exactly.
        .header(reqwest::header::AUTHORIZATION, format!("OAuth {access_token}"))
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Could not reach Twitch: {e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(error_message(status, &body));
    }
    serde_json::from_str::<Validated>(&body)
        .map_err(|e| format!("Twitch sent an unexpected validate response: {e}"))
}

/// Revoke a token server-side. Best effort: the local session is dropped either way, because a
/// failed revoke is not a reason to keep the user logged in against their wish.
pub async fn revoke(http: &reqwest::Client, client_id: &str, token: &str) -> Result<(), String> {
    let resp = http
        .post(REVOKE_URL)
        .form(&[("client_id", client_id), ("token", token)])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Could not reach Twitch: {e}"))?;
    // Documented: 200 with an empty body on success, 400 for an invalid token, 404 for an unknown
    // client. A 400 for an already-dead token is the desired end state, so it is not an error.
    let status = resp.status();
    if status.is_success() || status.as_u16() == 400 {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(error_message(status, &body))
    }
}

/// Shared parsing for both the grant and the refresh, which return the same shape.
///
/// Note `scope` is an **array** in the response even though it is a space-separated string in the
/// request. Twitch has returned both shapes over time, so accept either.
fn parse_token_set(body: &str) -> Result<TokenSet, String> {
    #[derive(Deserialize)]
    struct Raw {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: i64,
        #[serde(default)]
        scope: Option<Scopes>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Scopes {
        List(Vec<String>),
        One(String),
    }

    let raw: Raw = serde_json::from_str(body)
        .map_err(|e| format!("Twitch sent an unexpected token response: {e}"))?;
    let scopes = match raw.scope {
        Some(Scopes::List(v)) => v,
        Some(Scopes::One(s)) => s.split_whitespace().map(str::to_owned).collect(),
        None => Vec::new(),
    };
    Ok(TokenSet {
        access_token: raw.access_token,
        refresh_token: raw.refresh_token,
        scopes,
        expires_at: now_secs() + raw.expires_in.max(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_interval_is_floored_and_capped() {
        let mut d = DeviceCode {
            device_code: "x".into(),
            user_code: "ABCD".into(),
            verification_uri: "https://www.twitch.tv/activate".into(),
            expires_in: 1800,
            interval: 5,
        };
        assert_eq!(d.poll_interval(), Duration::from_secs(5));
        // A server-supplied 0 must not spin the loop.
        d.interval = 0;
        assert_eq!(d.poll_interval(), Duration::from_secs(1));
        d.interval = 9999;
        assert_eq!(d.poll_interval(), Duration::from_secs(60));
        // Negative would underflow a `u64` cast without the clamp.
        d.interval = -3;
        assert_eq!(d.poll_interval(), Duration::from_secs(1));
    }

    /// The single-use refresh token: dropping the rotated value leaves the stored one already
    /// invalidated, and the *next* refresh 401s in a way that looks like a revoked login.
    #[test]
    fn merge_refresh_keeps_the_rotated_token() {
        let mut stored = TokenSet {
            access_token: "old".into(),
            refresh_token: Some("r1".into()),
            scopes: vec!["bits:read".into()],
            expires_at: 100,
        };
        stored.merge_refresh(TokenSet {
            access_token: "new".into(),
            refresh_token: Some("r2".into()),
            scopes: vec!["bits:read".into(), "user:read:chat".into()],
            expires_at: 500,
        });
        assert_eq!(stored.access_token, "new");
        assert_eq!(stored.refresh_token.as_deref(), Some("r2"));
        assert_eq!(stored.scopes.len(), 2);
        assert_eq!(stored.expires_at, 500);
    }

    /// A refresh response without a `refresh_token` must not wipe the one we hold — losing it
    /// would force a full re-login for no reason.
    #[test]
    fn merge_refresh_without_a_new_token_keeps_the_old_one() {
        let mut stored = TokenSet {
            access_token: "old".into(),
            refresh_token: Some("r1".into()),
            scopes: vec![],
            expires_at: 100,
        };
        stored.merge_refresh(TokenSet {
            access_token: "new".into(),
            refresh_token: None,
            scopes: vec![],
            expires_at: 200,
        });
        assert_eq!(stored.refresh_token.as_deref(), Some("r1"));
        assert_eq!(stored.access_token, "new");
    }

    #[test]
    fn expiry_leaves_room_for_the_round_trip() {
        let t = TokenSet {
            access_token: "a".into(),
            refresh_token: None,
            scopes: vec![],
            expires_at: 1_000,
        };
        assert!(!t.is_expired(900, 60), "60s of margin covers a 10s timeout");
        assert!(t.is_expired(950, 60), "inside the margin is already expired");
        assert!(t.is_expired(1_000, 0));
    }

    #[test]
    fn token_response_accepts_both_scope_shapes() {
        // Array form (the documented one).
        let a = parse_token_set(
            r#"{"access_token":"at","refresh_token":"rt","expires_in":3600,
                "scope":["user:read:chat","bits:read"],"token_type":"bearer"}"#,
        )
        .unwrap();
        assert_eq!(a.scopes, vec!["user:read:chat", "bits:read"]);
        assert_eq!(a.refresh_token.as_deref(), Some("rt"));

        // Space-separated string form (older responses, and what the request uses).
        let b = parse_token_set(
            r#"{"access_token":"at","expires_in":10,"scope":"user:read:chat bits:read"}"#,
        )
        .unwrap();
        assert_eq!(b.scopes, vec!["user:read:chat", "bits:read"]);
        assert!(b.refresh_token.is_none());
    }

    /// A response missing `access_token` must fail loudly rather than store an empty token that
    /// 401s on every later call.
    #[test]
    fn token_response_without_an_access_token_is_an_error() {
        assert!(parse_token_set(r#"{"expires_in":3600}"#).is_err());
        assert!(parse_token_set("not json").is_err());
    }

    #[test]
    fn expiry_is_absolute_not_relative() {
        let before = now_secs();
        let t = parse_token_set(r#"{"access_token":"a","expires_in":3600}"#).unwrap();
        assert!(t.expires_at >= before + 3600 && t.expires_at <= before + 3601);
    }

    /// The error envelope is `{"status":…,"message":…}`; anything else falls back to the raw body
    /// with the status, so an HTML error page cannot produce an empty message.
    #[test]
    fn error_messages_survive_both_envelopes() {
        assert_eq!(
            error_message(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"Bad Request","status":400,"message":"authorization_pending"}"#
            ),
            "authorization_pending"
        );
        assert_eq!(
            error_message(reqwest::StatusCode::FORBIDDEN, "<html>nope</html>"),
            "HTTP 403: <html>nope</html>"
        );
        assert_eq!(error_message(reqwest::StatusCode::BAD_GATEWAY, "   "), "HTTP 502");
    }

    /// Both sides of the refresh decision: a dead grant must be *rejected* so the UI offers a
    /// re-login, while a 429/5xx must stay *transient* so a working token is not thrown away.
    #[test]
    fn refresh_classification_keeps_working_tokens() {
        for status in [reqwest::StatusCode::BAD_REQUEST, reqwest::StatusCode::UNAUTHORIZED] {
            assert!(
                matches!(classify_refresh(status.as_u16()), RefreshError::Rejected(_)),
                "{status} must force a re-login"
            );
        }
        for status in [
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::BAD_GATEWAY,
        ] {
            assert!(
                matches!(classify_refresh(status.as_u16()), RefreshError::Transient(_)),
                "{status} must not discard the token"
            );
        }
    }

    /// Mirrors the branch in [`refresh`] so the rule above is testable without a socket.
    fn classify_refresh(status: u16) -> RefreshError {
        if matches!(status, 400 | 401 | 403) {
            RefreshError::Rejected("x".into())
        } else {
            RefreshError::Transient("x".into())
        }
    }

    /// The scope list is a contract with Twitch's review process: asking for more than the app
    /// uses is grounds for suspension, so pin it.
    #[test]
    fn scopes_are_the_expected_set() {
        assert_eq!(SCOPES.len(), 8);
        assert!(SCOPES.contains(&"user:read:chat"));
        assert!(SCOPES.contains(&"user:write:chat"));
        assert!(SCOPES.contains(&"channel:manage:redemptions"));
        assert!(SCOPES.contains(&"bits:read"));
        // No duplicates: they would be sent twice and inflate the consent screen.
        let mut sorted = SCOPES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), SCOPES.len());
    }
}
