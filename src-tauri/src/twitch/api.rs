//! The Helix REST client: the few endpoints the integration actually calls, plus the error and
//! rate-limit handling Twitch requires of every caller.
//!
//! Twitch's rules that shape this file:
//!
//! * Every request carries **both** `Authorization: Bearer <token>` **and** `Client-Id`. A missing
//!   `Client-Id` is one of the documented causes of a 401, which is a confusing failure to debug.
//! * Rate limiting is a **token bucket** (per client ID; per client ID *per user* when a user token
//!   is used). Exhausting it returns **429** with `Ratelimit-Reset`, a **Unix epoch in seconds** —
//!   not a "retry after N seconds" delta. Reading it as a delta is the classic bug here.
//! * **503** has a documented, specific remedy: *"If you receive an HTTP status code 503 (Service
//!   Unavailable) error, retry once."* One retry, not a retry storm.
//! * **401** is the signal to refresh, not `expires_in`: *"twitch recommends that apps reactively
//!   respond to HTTP status code 401"*. So [`HelixError::Unauthorized`] is a distinct variant
//!   rather than being folded into a generic failure.
//! * *"Some API endpoints may return HTTP 429 response codes for reasons unrelated to the general
//!   rate limit bucket. In these cases, you must parse the error message"* — which is why the
//!   message is carried on the error instead of being dropped.
//!
//! Like `auth.rs`, this module knows nothing about Tauri or SQLite.

use std::time::Duration;

use serde::{Deserialize, Serialize};

const HELIX: &str = "https://api.twitch.tv/helix";

/// Helix is a normal JSON API; 20s is generous for a single round trip.
const TIMEOUT: Duration = Duration::from_secs(20);

/// What the caller should do about a failure. Keeping this explicit means the retry policy lives
/// in one place instead of being re-derived from status codes at every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelixError {
    /// 401 — the access token is expired, revoked, or of the wrong type. Refresh and retry once.
    Unauthorized(String),
    /// 403 — authenticated but not allowed: a missing scope, or (for Channel Points) a broadcaster
    /// who is not an affiliate or partner. Retrying will never help.
    Forbidden(String),
    /// 429 — back off until `reset_at` (Unix seconds), then retry.
    RateLimited {
        reset_at: Option<i64>,
        message: String,
    },
    /// 5xx — retry **once**, per the documented 503 rule.
    Unavailable(String),
    NotFound(String),
    BadRequest(String),
    /// Never reached Twitch, or the reply was not JSON in the expected shape.
    Transport(String),
}

impl HelixError {
    /// Whether retrying the identical request could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, HelixError::Unavailable(_))
    }

    /// The message to show a user, without the plumbing.
    pub fn message(&self) -> &str {
        match self {
            HelixError::Unauthorized(m)
            | HelixError::Forbidden(m)
            | HelixError::Unavailable(m)
            | HelixError::NotFound(m)
            | HelixError::BadRequest(m)
            | HelixError::Transport(m) => m,
            HelixError::RateLimited { message, .. } => message,
        }
    }

    /// The not-an-affiliate case, which needs its own copy in the UI: it is not a bug and there is
    /// nothing to retry, but it is also not obvious from the API's wording ("The broadcaster is not
    /// a partner or affiliate.").
    pub fn is_not_affiliate(&self) -> bool {
        matches!(self, HelixError::Forbidden(m) if m.to_ascii_lowercase().contains("affiliate"))
    }
}

impl std::fmt::Display for HelixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

/// A Twitch user. Only the fields the integration uses — Helix returns a dozen more
/// (`view_count`, `broadcaster_type` is kept because Channel Points needs it, `created_at`, …) and
/// deserialising them all would couple us to fields we do not read.
///
/// Note the asymmetric rename: Twitch sends `display_name`/`profile_image_url`, and this struct is
/// also handed to the webview as part of a `Snapshot`, whose every other field is camelCase. So it
/// **reads** Twitch's snake_case and **writes** the UI's camelCase. Renaming both ways would break
/// parsing Helix's replies; renaming neither would leave one snake_case object in an otherwise
/// camelCase payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct TwitchUser {
    pub id: String,
    pub login: String,
    pub display_name: String,
    #[serde(default)]
    pub profile_image_url: Option<String>,
    /// `""` for a normal account, `affiliate` or `partner` otherwise. Channel Points requires one
    /// of the latter two, so this is worth carrying: it lets the UI warn *before* the streamer
    /// configures five rewards that can never be created.
    #[serde(default)]
    pub broadcaster_type: String,
}

impl TwitchUser {
    /// Whether Channel Points rewards can exist on this channel at all.
    pub fn can_use_channel_points(&self) -> bool {
        matches!(self.broadcaster_type.as_str(), "affiliate" | "partner")
    }
}

/// A borrowed client for one authenticated call. Cheap to build per request; it holds no
/// connection state of its own (`reqwest::Client` does the pooling).
pub struct Helix<'a> {
    http: &'a reqwest::Client,
    client_id: &'a str,
    token: &'a str,
}

impl<'a> Helix<'a> {
    pub fn new(http: &'a reqwest::Client, client_id: &'a str, token: &'a str) -> Self {
        Helix { http, client_id, token }
    }

    /// Send a request, applying Twitch's retry rule.
    ///
    /// The rule is specific and small: *"If you receive an HTTP status code 503 (Service
    /// Unavailable) error, retry once."* One immediate retry — no backoff, no loop — because a 503
    /// is Twitch shedding load and a second attempt is the whole remedy the documentation offers.
    /// A second failure is reported as-is rather than hidden behind a third try.
    async fn send(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<String, HelixError> {
        let err = match self.attempt(build()).await {
            Ok(body) => return Ok(body),
            Err(e) => e,
        };
        if err.is_retryable() {
            tracing::debug!(error = %err, "twitch: retrying once after a server error");
            return self.attempt(build()).await;
        }
        Err(err)
    }

    /// One request, with the response read exactly once and classified.
    async fn attempt(&self, req: reqwest::RequestBuilder) -> Result<String, HelixError> {
        let resp = req
            .send()
            .await
            .map_err(|e| HelixError::Transport(format!("Could not reach Twitch: {e}")))?;
        let (status, headers, body) = split(resp).await;
        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(classify(status, &headers, &body))
        }
    }

    /// Every Helix call carries both headers. A missing `Client-Id` is one of the documented causes
    /// of a 401, so it is added in one place rather than at each call site.
    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header("Client-Id", self.client_id)
            .timeout(TIMEOUT)
    }

    /// `GET /helix/users?login=…` — resolve a channel name to its numeric id.
    ///
    /// This is how "select your channel" works. It needs no scope beyond a valid user token, and
    /// the numeric id is what every EventSub condition and most Helix parameters require (they
    /// take ids, not logins).
    pub async fn user_by_login(&self, login: &str) -> Result<Option<TwitchUser>, HelixError> {
        let login = normalize_login(login)?;
        let url = format!("{HELIX}/users");
        let body = self
            .send(|| self.authed(self.http.get(&url)).query(&[("login", login.as_str())]))
            .await?;
        let page: DataPage<TwitchUser> = serde_json::from_str(&body)
            .map_err(|e| HelixError::Transport(format!("Twitch sent an unexpected reply: {e}")))?;
        // An unknown login is an empty list, not a 404 — Helix's documented behaviour, and the
        // difference matters because "channel not found" is a typo the user can fix.
        Ok(page.data.into_iter().next())
    }

    /// `GET /helix/users` with no parameters — who the current token belongs to.
    pub async fn current_user(&self) -> Result<Option<TwitchUser>, HelixError> {
        let url = format!("{HELIX}/users");
        let body = self.send(|| self.authed(self.http.get(&url))).await?;
        let page: DataPage<TwitchUser> = serde_json::from_str(&body)
            .map_err(|e| HelixError::Transport(format!("Twitch sent an unexpected reply: {e}")))?;
        Ok(page.data.into_iter().next())
    }

    /// `POST /helix/eventsub/subscriptions` — subscribe this WebSocket session to one event type.
    ///
    /// The JSON shape is fixed by Twitch: a `condition` naming what to watch, and a `transport`
    /// carrying the session id from `session_welcome`. A subscription created any other way cannot
    /// be attached to a WebSocket.
    ///
    /// This is on the critical path: it has to land within 10 seconds of the welcome or the server
    /// closes the connection with 4003. So it deliberately does not retry beyond the single 5xx
    /// retry inside [`Helix::send`].
    pub async fn subscribe(
        &self,
        kind: &str,
        condition: serde_json::Value,
        session_id: &str,
    ) -> Result<Subscription, HelixError> {
        let url = format!("{HELIX}/eventsub/subscriptions");
        let body = serde_json::json!({
            "type": kind,
            "version": "1",
            "condition": condition,
            "transport": {
                "method": "websocket",
                "session_id": session_id,
            },
        });
        let text = self.send(|| self.authed(self.http.post(&url)).json(&body)).await?;
        let page: DataPage<Subscription> = serde_json::from_str(&text)
            .map_err(|e| HelixError::Transport(format!("Twitch sent an unexpected reply: {e}")))?;
        page.data.into_iter().next().ok_or_else(|| {
            HelixError::Transport("Twitch accepted the subscription but returned none".into())
        })
    }

    /// Chat, which needs both the channel and the user the token belongs to — Twitch's "The User ID
    /// to read chat as", which is what makes the difference between reading chat and not.
    pub async fn subscribe_chat(
        &self,
        session_id: &str,
        broadcaster_id: &str,
        user_id: &str,
    ) -> Result<Subscription, HelixError> {
        self.subscribe(
            "channel.chat.message",
            serde_json::json!({
                "broadcaster_user_id": broadcaster_id,
                "user_id": user_id,
            }),
            session_id,
        )
        .await
    }

    /// Channel Points redemptions for one reward.
    ///
    /// The condition is the channel and the reward, and there is no `user_id`: a redemption is the
    /// broadcaster's to see, so there is nobody to read it as. Naming the reward narrows what
    /// arrives to the one that matters, which is worth doing — the alternative is hearing about
    /// every redemption of every reward in the channel and discarding almost all of them.
    pub async fn subscribe_redemptions(
        &self,
        session_id: &str,
        broadcaster_id: &str,
        reward_id: &str,
    ) -> Result<Subscription, HelixError> {
        self.subscribe(
            "channel.channel_points_custom_reward_redemption.add",
            serde_json::json!({
                "broadcaster_user_id": broadcaster_id,
                "reward_id": reward_id,
            }),
            session_id,
        )
        .await
    }

    /// `POST /helix/chat/messages` — say something in the channel.
    ///
    /// **A 200 does not mean it was sent.** Twitch answers `is_sent: false` with a `drop_reason`
    /// when the message was refused — the bot is banned, timed out, the channel is in
    /// followers-only mode and it does not qualify, or the message broke a rule — and the HTTP
    /// status is the same either way. A caller that only checks the status reads a silent failure as
    /// success, which on stream looks like a bot that ignores people.
    ///
    /// `sender_id` is the account the token belongs to, not the broadcaster: a moderator's token
    /// sends as the moderator. Here they are the same account, which is why the session passes its
    /// own user id.
    #[allow(dead_code)]
    pub async fn send_chat_message(
        &self,
        broadcaster_id: &str,
        sender_id: &str,
        message: &str,
    ) -> Result<(), HelixError> {
        // Twitch's own limit. Sending more is refused with a drop reason rather than truncated, so
        // it is worth not asking — and the module that writes replies already caps them below this.
        if message.chars().count() > 500 {
            return Err(HelixError::Transport(format!(
                "refusing to send {} characters; Twitch's limit is 500",
                message.chars().count()
            )));
        }

        let url = format!("{HELIX}/chat/messages");
        let body = serde_json::json!({
            "broadcaster_id": broadcaster_id,
            "sender_id": sender_id,
            "message": message,
        });
        let text = self.send(|| self.authed(self.http.post(&url)).json(&body)).await?;

        #[derive(serde::Deserialize)]
        struct Sent {
            #[serde(default)]
            is_sent: bool,
            #[serde(default)]
            drop_reason: Option<DropReason>,
        }
        #[derive(serde::Deserialize)]
        struct DropReason {
            #[serde(default)]
            code: String,
            #[serde(default)]
            message: String,
        }
        #[derive(serde::Deserialize)]
        struct SentPage {
            #[serde(default)]
            data: Vec<Sent>,
        }

        let page: SentPage = serde_json::from_str(&text)
            .map_err(|e| HelixError::Transport(format!("Twitch sent an unexpected reply: {e}")))?;
        match page.data.into_iter().next() {
            Some(s) if s.is_sent => Ok(()),
            Some(s) => {
                let reason =
                    s.drop_reason.map(|d| d.message).unwrap_or_else(|| "no reason given".into());
                Err(HelixError::Transport(format!("Twitch dropped the message: {reason}")))
            }
            None => Err(HelixError::Transport(
                "Twitch accepted the send but reported no message".into(),
            )),
        }
    }
}

/// One EventSub subscription, as Helix reports it.
///
/// Only the fields the app acts on. `cost` matters because the budget for a WebSocket session is
/// **10** — not the 10,000 that appears in the webhook examples — so knowing what a subscription
/// costs is what says whether the next one will fit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct Subscription {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub cost: i64,
}

/// Clean up a user-entered channel name into what Helix's `login` parameter wants.
///
/// Users paste `@name` or a full URL, and the parameter matches logins case-insensitively but does
/// not accept the `@`. Pure, so the fiddly part is testable without a socket.
fn normalize_login(raw: &str) -> Result<String, HelixError> {
    let mut s = raw.trim();
    // A pasted profile URL: keep the last path segment, dropping any trailing slash or query.
    if let Some(rest) = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .and_then(|r| r.split_once("twitch.tv/"))
        .map(|(_, path)| path)
    {
        s = rest.split(['/', '?']).next().unwrap_or("");
    }
    let s = s.trim().trim_start_matches('@').trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err(HelixError::BadRequest("Enter a channel name.".into()));
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(HelixError::BadRequest(
            "A Twitch channel name only contains letters, numbers and underscores.".into(),
        ));
    }
    Ok(s)
}

/// Helix wraps every list response in a `data` array.
#[derive(Debug, Deserialize)]
struct DataPage<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

/// The rate-limit headers, pulled out before the body is consumed.
#[derive(Debug, Default, Clone, Copy)]
struct RateHeaders {
    limit: Option<i64>,
    remaining: Option<i64>,
    /// Unix epoch seconds, per the docs — not a delta.
    reset_at: Option<i64>,
}

impl RateHeaders {
    fn read(h: &reqwest::header::HeaderMap) -> Self {
        let get = |name: &str| {
            h.get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok())
        };
        RateHeaders {
            limit: get("ratelimit-limit"),
            remaining: get("ratelimit-remaining"),
            reset_at: get("ratelimit-reset"),
        }
    }
}

/// Split a response into what the classifier needs, so the body is read exactly once.
async fn split(resp: reqwest::Response) -> (u16, RateHeaders, String) {
    let status = resp.status().as_u16();
    let headers = RateHeaders::read(resp.headers());
    let body = resp.text().await.unwrap_or_default();
    (status, headers, body)
}

/// Map an HTTP status onto the retry policy. Kept separate from the request so the rule table is
/// testable without a socket.
fn classify(status: u16, headers: &RateHeaders, body: &str) -> HelixError {
    let message = envelope_message(status, body);
    match status {
        401 => HelixError::Unauthorized(message),
        403 => HelixError::Forbidden(message),
        404 => HelixError::NotFound(message),
        429 => {
            // The docs warn that a 429 can also arrive "for reasons unrelated to the general rate
            // limit bucket", so the log line carries the budget. `remaining: 0` means we spent it;
            // `remaining: 500` means Twitch throttled us anyway, which is a different problem and
            // would be invisible without these two numbers.
            tracing::warn!(
                limit = headers.limit,
                remaining = headers.remaining,
                "twitch: rate limited, retry at {:?}",
                headers.reset_at
            );
            HelixError::RateLimited { reset_at: headers.reset_at, message }
        }
        // 500 is retryable in practice even though only 503 has a documented remedy.
        500 | 502 | 503 | 504 => HelixError::Unavailable(message),
        s if (400..500).contains(&s) => HelixError::BadRequest(message),
        _ => HelixError::Unavailable(message),
    }
}

/// Twitch's error envelope is `{"error":"…","status":…,"message":"…"}`. Fall back to the raw body
/// so an HTML error page still produces something readable.
fn envelope_message(status: u16, body: &str) -> String {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        message: String,
    }
    if let Ok(e) = serde_json::from_str::<Envelope>(body) {
        if !e.message.is_empty() {
            return e.message;
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        format!("Twitch returned HTTP {status}")
    } else {
        format!("Twitch returned HTTP {status}: {}", trimmed.chars().take(200).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(limit: &str, remaining: &str, reset: &str) -> RateHeaders {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("ratelimit-limit", limit.parse().unwrap());
        h.insert("ratelimit-remaining", remaining.parse().unwrap());
        h.insert("ratelimit-reset", reset.parse().unwrap());
        RateHeaders::read(&h)
    }

    /// `Ratelimit-Reset` is an **epoch**, and the only place the rule can be checked without a
    /// socket is here. A 429 handler that treated it as a delta would wait ~1.7 billion seconds;
    /// the value below is the one from Twitch's own documented example response.
    #[test]
    fn rate_headers_are_read_from_the_documented_names() {
        let h = headers("800", "799", "1781653392");
        assert_eq!(h.limit, Some(800));
        assert_eq!(h.remaining, Some(799));
        assert_eq!(h.reset_at, Some(1_781_653_392));
        // Sanity: the documented value is a plausible "now"-shaped epoch, not a small delta.
        assert!(h.reset_at.unwrap() > 1_000_000_000);
    }

    /// A garbage or absent header must not poison the request: it degrades to `None`.
    #[test]
    fn rate_headers_tolerate_junk_and_absence() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("ratelimit-reset", "not-a-number".parse().unwrap());
        let r = RateHeaders::read(&h);
        assert_eq!(r.reset_at, None);
        assert_eq!(r.limit, None);
    }

    /// A 429 without the header is explicitly possible ("for reasons unrelated to the general rate
    /// limit bucket"), so the error has to carry "no instruction" rather than inventing a delay.
    /// Whoever adds the backoff in phase 3 reads `reset_at: None` as exactly that.
    #[test]
    fn rate_limit_without_a_reset_header_has_no_instruction() {
        let err = classify(429, &RateHeaders::default(), r#"{"message":"slow down"}"#);
        match &err {
            HelixError::RateLimited { reset_at, message } => {
                assert_eq!(*reset_at, None);
                assert_eq!(message, "slow down");
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    /// The retry rule: 5xx is retryable, everything else is not. Retrying a 403 or a 401 against
    /// the same credentials can never succeed.
    #[test]
    fn only_server_errors_are_retryable() {
        assert!(classify(503, &RateHeaders::default(), "").is_retryable());
        assert!(classify(500, &RateHeaders::default(), "").is_retryable());
        assert!(!classify(401, &RateHeaders::default(), "").is_retryable());
        assert!(!classify(403, &RateHeaders::default(), "").is_retryable());
        assert!(!classify(429, &RateHeaders::default(), "").is_retryable());
        assert!(!classify(404, &RateHeaders::default(), "").is_retryable());
    }

    /// 401 must be its own variant: it is the documented trigger for a token refresh, so folding
    /// it into a generic error would lose the one signal the refresh logic runs on.
    #[test]
    fn unauthorized_is_distinguishable() {
        assert!(matches!(
            classify(401, &RateHeaders::default(), r#"{"message":"invalid access token"}"#),
            HelixError::Unauthorized(_)
        ));
    }

    /// Channel Points on a non-affiliate is a 403 with a specific message, and the UI needs to
    /// recognise it to explain instead of showing a bare failure.
    #[test]
    fn not_an_affiliate_is_recognised() {
        let err = classify(
            403,
            &RateHeaders::default(),
            r#"{"error":"Forbidden","status":403,"message":"The broadcaster is not a partner or affiliate."}"#,
        );
        assert!(err.is_not_affiliate());
        let other = classify(403, &RateHeaders::default(), r#"{"message":"missing scope"}"#);
        assert!(!other.is_not_affiliate());
    }

    /// The envelope is stripped when present; the raw body is kept when it is not.
    #[test]
    fn envelope_message_prefers_the_message_field() {
        assert_eq!(
            envelope_message(403, r#"{"error":"Forbidden","status":403,"message":"nope"}"#),
            "nope"
        );
        assert_eq!(
            envelope_message(502, "<html>bad gateway</html>"),
            "Twitch returned HTTP 502: <html>bad gateway</html>"
        );
        assert_eq!(envelope_message(500, "  "), "Twitch returned HTTP 500");
    }

    #[test]
    fn user_page_parses_and_survives_extra_fields() {
        let page: DataPage<TwitchUser> = serde_json::from_str(
            r#"{"data":[{"id":"12826","login":"twitch","display_name":"Twitch",
                 "profile_image_url":"https://x/y.png","broadcaster_type":"partner",
                 "view_count":123456,"created_at":"2007-05-22T10:39:54Z","type":""}]}"#,
        )
        .unwrap();
        let u = &page.data[0];
        assert_eq!(u.id, "12826");
        assert_eq!(u.login, "twitch");
        assert!(u.can_use_channel_points());
    }

    /// An unknown login is an empty list, which the caller turns into `None` — not an error.
    #[test]
    fn empty_data_is_not_a_failure() {
        let page: DataPage<TwitchUser> = serde_json::from_str(r#"{"data":[]}"#).unwrap();
        assert!(page.data.is_empty());
    }

    /// A normal account has `broadcaster_type: ""`, and Channel Points endpoints 403 for it.
    /// Affiliates and partners are the only ones that can own custom rewards.
    #[test]
    fn a_plain_account_cannot_use_channel_points() {
        let user = |kind: &str| TwitchUser {
            id: "1".into(),
            login: "someone".into(),
            display_name: "Someone".into(),
            profile_image_url: None,
            broadcaster_type: kind.into(),
        };
        assert!(!user("").can_use_channel_points(), "a normal account");
        assert!(user("affiliate").can_use_channel_points());
        assert!(user("partner").can_use_channel_points());
    }

    /// Channel names arrive with an `@`, with capitals, and sometimes as a pasted URL. Helix wants
    /// a bare lowercase login and rejects anything else.
    #[test]
    fn login_is_normalised() {
        assert_eq!(normalize_login("Shroud").unwrap(), "shroud");
        assert_eq!(normalize_login("  @Shroud  ").unwrap(), "shroud");
        assert_eq!(normalize_login("https://www.twitch.tv/Shroud/").unwrap(), "shroud");
        assert_eq!(normalize_login("https://twitch.tv/shroud?sr=a").unwrap(), "shroud");
        // A login may contain an underscore but not a hyphen or a space.
        assert_eq!(normalize_login("some_one").unwrap(), "some_one");
    }

    /// Garbage is rejected *before* a request is built, so the user gets a sentence instead of an
    /// empty result that looks like "this channel does not exist".
    #[test]
    fn unusable_logins_are_rejected_locally() {
        for bad in ["", "   ", "@", "not a name", "has-hyphen", "emoji😀"] {
            assert!(
                matches!(normalize_login(bad), Err(HelixError::BadRequest(_))),
                "{bad:?} should be rejected"
            );
        }
    }
}
