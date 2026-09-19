//! EventSub over WebSocket: the protocol layer.
//!
//! One connection to `wss://eventsub.wss.twitch.tv/ws` carries every subscription the app has, so
//! this file knows about envelopes, sessions and the shapes Twitch sends — and nothing about
//! tokens, subscriptions or the music. The loop that owns those lives in `mod.rs`, because the
//! token does.
//!
//! ## What Twitch requires of a client, and where each rule is honoured
//!
//! | rule | here |
//! |---|---|
//! | Subscribe within 10s of `session_welcome`, or the server closes with 4003 | `mod.rs`, which sends `Welcome` straight to the subscribe call |
//! | Answer Ping frames or be closed with 4002 | **handled by `tungstenite`**, see [`open`] |
//! | Send nothing but Pong | this module has no writer at all |
//! | `session_reconnect` arrives 30s before the close, use the URL as-is | surfaced as [`Incoming::Reconnect`] |
//! | Delivery is *at least once* — dedupe by `message_id` | [`Dedupe`] |
//! | `revocation` means the subscription is gone; it is not retried blindly | [`Incoming::Revoked`] |
//!
//! ## The one requirement deliberately not implemented
//!
//! Twitch's docs suggest rejecting notifications whose `message_timestamp` is more than ten minutes
//! old. That guard exists for **webhook** transports, where a captured request can be replayed at a
//! URL. Over a WebSocket there is no such surface: the connection is TLS-protected and
//! server-initiated, and a message cannot be injected into it. Implementing it would mean parsing
//! RFC3339 by hand — this crate has no date library, and pulling one in for a comparison that
//! guards nothing is the wrong trade. Duplicate *delivery* is a real problem and is handled, by
//! [`Dedupe`].

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Twitch's EventSub WebSocket endpoint. `/ws` with no path suffix; the reconnect URL is a
/// different host and must be used verbatim.
pub const DEFAULT_URL: &str = "wss://eventsub.wss.twitch.tv/ws";

/// The subscription type this phase listens to.
pub const CHAT_MESSAGE: &str = "channel.chat.message";

/// How long one resolved address gets to complete its TCP handshake before we move to the next.
///
/// Copied from `listentogether/mod.rs`, where the reasoning was worked out the hard way:
/// `tokio_tungstenite::connect_async` hands `host:port` to `TcpStream::connect`, which walks the
/// resolved addresses one at a time and waits out the OS SYN timeout (~127s on Linux) on each dead
/// one. A host with a black-holed AAAA record therefore burns minutes before trying the IPv4 that
/// works. Four seconds per address turns that into a fall-through.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// How many recent `message_id`s to remember. Twitch redelivers rarely and close together, so a
/// few hundred covers any plausible redelivery window without growing without bound on a busy
/// channel.
const DEDUPE_WINDOW: usize = 512;

/// The EventSub socket type, named so callers can hold one without repeating three generics.
pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The parsed `metadata` of every frame Twitch sends.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Metadata {
    /// Stable across redeliveries of the same event, which is what makes it the dedupe key.
    pub message_id: String,
    /// `session_welcome`, `session_keepalive`, `session_reconnect`, `notification`, `revocation`.
    pub message_type: String,
    /// RFC3339 with nanoseconds. Carried through for logging; see the module docs for why it is
    /// not validated.
    #[serde(default)]
    pub message_timestamp: String,
}

/// The outer envelope. `payload` is left as a [`Value`] because its shape depends entirely on
/// `message_type`, and parsing it twice would mean two structs per type.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    pub metadata: Metadata,
    #[serde(default)]
    pub payload: Value,
}

/// A parsed frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The first frame of a session. Carries the id every subscription must reference, and the
    /// deadline that comes with it.
    Welcome(Welcome),
    /// No notification arrived inside the keepalive window. Nothing to do but note it: the fact
    /// that it arrived at all is the liveness signal.
    Keepalive,
    /// A subscribed event fired.
    Notification(Notification),
    /// Twitch is about to close this socket. Connect to the URL, and **do not close the old socket
    /// until the new one has sent its welcome** — that is what makes the handover seamless.
    Reconnect { url: String },
    /// A subscription was revoked and will not fire again.
    Revoked { status: String, kind: String },
    /// A frame type this build does not know. Not an error: Twitch adds types over time, and the
    /// documented behaviour is to ignore what you do not understand rather than tear down the
    /// connection.
    Unknown { message_type: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Welcome {
    /// The session id every subscription in this connection must carry.
    pub session_id: String,
    /// How long Twitch will stay quiet before sending a keepalive. 10s unless we asked otherwise.
    pub keepalive_timeout_seconds: i64,
    /// Present when this is a reconnect, absent on a fresh connection.
    pub reconnect_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub message_id: String,
    /// The `subscription.type` that produced this, e.g. `channel.chat.message`.
    pub kind: String,
    pub version: String,
    /// The `event` object, still unparsed — the caller knows which type it asked for.
    pub event: Value,
}

impl Envelope {
    /// Turn an envelope into something the caller can match on.
    ///
    /// Returns `Ok(None)` for a frame that is well-formed but carries nothing actionable, which is
    /// different from an error: a missing payload on a `notification` is a bug, a missing payload
    /// on a type we do not know is expected.
    pub fn into_incoming(self) -> Result<Option<Incoming>, String> {
        let Envelope { metadata, payload } = self;
        let incoming = match metadata.message_type.as_str() {
            "session_welcome" => {
                let session =
                    payload.get("session").ok_or("session_welcome carried no session object")?;
                let session_id = session
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("session_welcome carried no session id")?
                    .to_owned();
                Incoming::Welcome(Welcome {
                    session_id,
                    // Absent means Twitch's default, which is 10 seconds.
                    keepalive_timeout_seconds: session
                        .get("keepalive_timeout_seconds")
                        .and_then(Value::as_i64)
                        .unwrap_or(10),
                    reconnect_url: session
                        .get("reconnect_url")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            }
            "session_keepalive" => Incoming::Keepalive,
            "session_reconnect" => {
                let url = payload
                    .get("session")
                    .and_then(|s| s.get("reconnect_url"))
                    .and_then(Value::as_str)
                    .ok_or("session_reconnect carried no reconnect url")?
                    .to_owned();
                Incoming::Reconnect { url }
            }
            "notification" => {
                let subscription = payload
                    .get("subscription")
                    .ok_or("notification carried no subscription object")?;
                Incoming::Notification(Notification {
                    message_id: metadata.message_id,
                    kind: subscription
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    version: subscription
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    event: payload.get("event").cloned().unwrap_or(Value::Null),
                })
            }
            "revocation" => {
                let subscription = payload.get("subscription");
                Incoming::Revoked {
                    status: subscription
                        .and_then(|s| s.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    kind: subscription
                        .and_then(|s| s.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                }
            }
            // Includes `session_keepalive`-adjacent types Twitch adds later. Ignoring is the
            // documented stance; closing the socket over one would be a self-inflicted outage.
            other => Incoming::Unknown { message_type: other.to_owned() },
        };
        Ok(Some(incoming))
    }
}

/// One chat badge. Kept raw: what a badge *means* for permissions is `permissions.rs`'s business
/// in phase 3, and inventing a role here would put policy in the protocol layer.
///
/// The asymmetric rename is the same one `api::TwitchUser` needs: Twitch sends `set_id`, and this
/// is handed to the webview inside an otherwise camelCase payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct Badge {
    pub set_id: String,
    pub id: String,
    #[serde(default)]
    pub info: String,
}

/// A `channel.chat.message` event, reduced to the fields this app has a use for.
///
/// Field names are Twitch's, verified against the EventSub reference; serialisation to the UI is
/// camelCase, which is why the rename is asymmetric — the same reasoning as `api::TwitchUser`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase"))]
pub struct ChatMessage {
    /// A UUID. The dedupe key for a redelivered event.
    pub message_id: String,
    pub chatter_user_id: String,
    pub chatter_user_login: String,
    pub chatter_user_name: String,
    /// The plain-text body. Fragments carry emote metadata we do not render yet.
    pub text: String,
    #[serde(default)]
    pub badges: Vec<Badge>,
    /// Present only when the message is a cheer.
    #[serde(default)]
    pub bits: Option<i64>,
    /// Present when the message came from a Channel Points redemption — the link phase 4 needs.
    #[serde(default)]
    pub reward_id: Option<String>,
    /// Whether this is a reply, so the tail can mark it without carrying the parent body.
    #[serde(default)]
    pub is_reply: bool,
}

/// The raw shape of the `channel.chat.message` event object. Separate from [`ChatMessage`] because
/// Twitch nests `text` inside `message` and `bits` inside `cheer`, and flattening that in serde
/// would need a custom deserializer for two fields.
#[derive(Debug, Deserialize)]
struct RawChatMessage {
    message_id: String,
    chatter_user_id: String,
    chatter_user_login: String,
    chatter_user_name: String,
    message: RawMessageBody,
    #[serde(default)]
    badges: Vec<Badge>,
    #[serde(default)]
    cheer: Option<RawCheer>,
    #[serde(default)]
    channel_points_custom_reward_id: Option<String>,
    #[serde(default)]
    reply: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RawMessageBody {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct RawCheer {
    #[serde(default)]
    bits: i64,
}

impl ChatMessage {
    /// Parse the `event` object of a `channel.chat.message` notification.
    pub fn from_event(event: &Value) -> Result<Self, String> {
        let raw: RawChatMessage = serde_json::from_value(event.clone())
            .map_err(|e| format!("unexpected channel.chat.message payload: {e}"))?;
        Ok(ChatMessage {
            message_id: raw.message_id,
            chatter_user_id: raw.chatter_user_id,
            chatter_user_login: raw.chatter_user_login,
            chatter_user_name: raw.chatter_user_name,
            text: raw.message.text,
            badges: raw.badges,
            bits: raw.cheer.map(|c| c.bits),
            reward_id: raw.channel_points_custom_reward_id,
            is_reply: raw.reply.is_some(),
        })
    }

    /// The badge for a badge set, e.g. `badge("moderator")`.
    ///
    /// Test-only: the UI reads `badges` directly, and phase 3's role mapping will live in
    /// `permissions.rs` rather than as an accessor here.
    #[cfg(test)]
    pub fn badge(&self, set_id: &str) -> Option<&Badge> {
        self.badges.iter().find(|b| b.set_id == set_id)
    }
}

/// Remembers recently seen `message_id`s so a redelivered event is acted on once.
///
/// Twitch delivers *at least once*: the same event can arrive twice, and when it does the
/// `message_id` is identical. A bounded window rather than an unbounded set, because a busy channel
/// would otherwise grow this for the life of the process.
#[derive(Debug, Default)]
pub struct Dedupe {
    seen: HashSet<String>,
    order: VecDeque<String>,
    dropped: u64,
}

impl Dedupe {
    pub fn new() -> Self {
        Dedupe::default()
    }

    /// `true` the first time an id is seen, `false` for every repeat. An id seen long enough ago to
    /// have fallen out of the window counts as new, which is the right failure direction: acting
    /// twice on a very old duplicate is better than ignoring a genuinely new event that happens to
    /// reuse an id.
    pub fn accept(&mut self, message_id: &str) -> bool {
        if self.seen.contains(message_id) {
            self.dropped += 1;
            return false;
        }
        self.seen.insert(message_id.to_owned());
        self.order.push_back(message_id.to_owned());
        while self.order.len() > DEDUPE_WINDOW {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    /// How many duplicates have been dropped. Surfaced in the UI: a counter that never moves is how
    /// you tell "dedupe is working" from "dedupe is never exercised".
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The window's current size. Test-only: the loop drops the whole `Dedupe` when the channel
    /// changes, so nothing in the app needs to inspect it, and the bound is only observable from a
    /// test.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.order.len()
    }
}

/// Open the EventSub socket.
///
/// **Ping/Pong is not handled here on purpose.** `tungstenite` queues the Pong for a received Ping
/// as part of reading, and `tokio-tungstenite` flushes it, so a correct Pong goes out without this
/// module owning a writer. That matters beyond brevity: Twitch's rule is *"if you send a message to
/// the server, except for Pong messages, the server closes the connection"*, and a module with no
/// way to write cannot break it.
pub async fn open(url: &str) -> Result<Socket, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let req = url.into_client_request().map_err(|e| format!("bad EventSub URL: {e}"))?;
    let uri = req.uri().clone();
    let host = uri.host().ok_or("EventSub URL has no host")?.to_owned();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("ws") { 80 } else { 443 });

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("could not resolve {host}: {e}"))?
        .collect();
    let socket = connect_first(&addrs).await.map_err(|e| format!("{host}: {e}"))?;

    let (ws, _) = tokio_tungstenite::client_async_tls_with_config(req, socket, None, None)
        .await
        .map_err(|e| format!("EventSub handshake failed: {e}"))?;
    Ok(ws)
}

/// First address that completes a handshake within [`CONNECT_TIMEOUT`], in order.
async fn connect_first(addrs: &[SocketAddr]) -> std::io::Result<TcpStream> {
    let mut last: Option<std::io::Error> = None;
    for &addr in addrs {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => return Ok(s),
            Ok(Err(e)) => {
                tracing::debug!(%addr, error = %e, "twitch: EventSub address refused");
                last = Some(e);
            }
            Err(_) => {
                tracing::debug!(%addr, "twitch: EventSub address timed out");
                last = Some(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{addr} did not answer in {CONNECT_TIMEOUT:?}"),
                ));
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "host resolved to nothing")
    }))
}

/// Exponential backoff, capped at 32s — the same curve `listentogether/mod.rs` uses.
pub fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.clamp(1, 6) - 1; // 0..=5 → 1,2,4,8,16,32
    Duration::from_secs(1u64 << shift)
}

/// Read one frame and parse it. `None` means the stream ended, which the caller treats as a
/// disconnect worth retrying.
pub async fn next(ws: &mut Socket) -> Option<Result<Incoming, String>> {
    loop {
        match ws.next().await? {
            Ok(Message::Text(t)) => match serde_json::from_str::<Envelope>(&t) {
                Ok(env) => match env.into_incoming() {
                    Ok(Some(incoming)) => return Some(Ok(incoming)),
                    // A frame with nothing actionable: keep reading rather than surfacing it.
                    Ok(None) => continue,
                    Err(e) => return Some(Err(e)),
                },
                // A frame we cannot parse is not fatal: log it and keep the session, because the
                // alternative is dropping a working connection over one bad message.
                Err(e) => {
                    tracing::debug!(error = %e, "twitch: unparseable EventSub frame");
                    continue;
                }
            },
            // Close and transport errors end the read loop; the caller reconnects.
            Ok(Message::Close(frame)) => {
                return Some(Err(close_reason(frame)));
            }
            Err(e) => return Some(Err(format!("EventSub socket error: {e}"))),
            // Ping/Pong/Binary/Frame: nothing to do, and answering a Ping is tungstenite's job.
            _ => continue,
        }
    }
}

/// Twitch's close codes, from the EventSub documentation. Named because "1006" tells a user
/// nothing and these are the difference between "retry" and "your subscription is wrong".
fn close_reason(frame: Option<tokio_tungstenite::tungstenite::protocol::CloseFrame>) -> String {
    let Some(frame) = frame else {
        return "EventSub connection closed".into();
    };
    let code = u16::from(frame.code);
    let what = match code {
        4000 => "internal server error",
        4001 => "we sent the server something it does not accept",
        4002 => "the server did not get a pong in time",
        4003 => "the connection was never used (no subscription in time)",
        4004 => "the reconnect window expired",
        4005 => "network timeout",
        4006 => "network error",
        4007 => "invalid reconnect",
        _ => "closed",
    };
    format!("EventSub {what} ({code})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(json: &str) -> Envelope {
        serde_json::from_str(json).expect("test fixture must parse")
    }

    /// A real `session_welcome`. The session id is what every subscription is keyed on, so it must
    /// survive parsing exactly.
    #[test]
    fn welcome_carries_the_session_id_and_keepalive() {
        let env = envelope(
            r#"{"metadata":{"message_id":"abc","message_type":"session_welcome",
                 "message_timestamp":"2026-09-19T00:00:00.000000000Z"},
                "payload":{"session":{"id":"AgoQ123","status":"enabled",
                 "connected_at":"2026-09-19T00:00:00.000000000Z",
                 "keepalive_timeout_seconds":10}}}"#,
        );
        match env.into_incoming().unwrap().unwrap() {
            Incoming::Welcome(w) => {
                assert_eq!(w.session_id, "AgoQ123");
                assert_eq!(w.keepalive_timeout_seconds, 10);
                assert_eq!(w.reconnect_url, None);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    /// `keepalive_timeout_seconds` is absent on some sessions; the documented default is 10s.
    #[test]
    fn welcome_defaults_the_keepalive_window() {
        let env = envelope(
            r#"{"metadata":{"message_id":"a","message_type":"session_welcome"},
                "payload":{"session":{"id":"S"}}}"#,
        );
        match env.into_incoming().unwrap().unwrap() {
            Incoming::Welcome(w) => assert_eq!(w.keepalive_timeout_seconds, 10),
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    /// A welcome with no session id is unusable — every subscribe would fail — so it must be an
    /// error rather than an empty id.
    #[test]
    fn welcome_without_a_session_id_is_an_error() {
        let env = envelope(
            r#"{"metadata":{"message_id":"a","message_type":"session_welcome"},"payload":{"session":{}}}"#,
        );
        assert!(env.into_incoming().is_err());
    }

    #[test]
    fn reconnect_surfaces_the_url_verbatim() {
        let env = envelope(
            r#"{"metadata":{"message_id":"a","message_type":"session_reconnect"},
                "payload":{"session":{"id":"S","reconnect_url":"wss://eventsub.wss.twitch.tv/ws?abc=1"}}}"#,
        );
        match env.into_incoming().unwrap().unwrap() {
            Incoming::Reconnect { url } => {
                // Must not be rebuilt from parts: Twitch's URL carries the query it needs.
                assert_eq!(url, "wss://eventsub.wss.twitch.tv/ws?abc=1")
            }
            other => panic!("expected Reconnect, got {other:?}"),
        }
    }

    #[test]
    fn keepalive_and_revocation_parse() {
        let ka = envelope(
            r#"{"metadata":{"message_id":"a","message_type":"session_keepalive"},"payload":{}}"#,
        );
        assert_eq!(ka.into_incoming().unwrap().unwrap(), Incoming::Keepalive);

        let rev = envelope(
            r#"{"metadata":{"message_id":"a","message_type":"revocation"},
                "payload":{"subscription":{"id":"1","status":"user_removed","type":"channel.chat.message","version":"1"}}}"#,
        );
        match rev.into_incoming().unwrap().unwrap() {
            Incoming::Revoked { status, kind } => {
                assert_eq!(status, "user_removed");
                assert_eq!(kind, "channel.chat.message");
            }
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    /// An `Unknown` type must not be an error: Twitch adds frame types over time and the
    /// documented stance is to ignore what you do not understand.
    #[test]
    fn unknown_message_types_are_ignored_not_fatal() {
        let env = envelope(
            r#"{"metadata":{"message_id":"a","message_type":"session_something_new"},"payload":{}}"#,
        );
        match env.into_incoming().unwrap().unwrap() {
            Incoming::Unknown { message_type } => assert_eq!(message_type, "session_something_new"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// A notification keeps the envelope's `message_id` (the dedupe key) and the subscription type.
    #[test]
    fn notification_keeps_the_dedupe_key_and_kind() {
        let env = envelope(
            r#"{"metadata":{"message_id":"dedupe-me","message_type":"notification"},
                "payload":{"subscription":{"id":"1","type":"channel.chat.message","version":"1","cost":0},
                           "event":{"message_id":"evt-1"}}}"#,
        );
        match env.into_incoming().unwrap().unwrap() {
            Incoming::Notification(n) => {
                assert_eq!(n.message_id, "dedupe-me");
                assert_eq!(n.kind, CHAT_MESSAGE);
                assert_eq!(n.version, "1");
                assert_eq!(n.event.get("message_id").unwrap(), "evt-1");
            }
            other => panic!("expected Notification, got {other:?}"),
        }
    }

    /// A notification with no `subscription` object is a protocol violation, not something to
    /// shrug at: it would produce an event with no type.
    #[test]
    fn notification_without_a_subscription_is_an_error() {
        let env = envelope(
            r#"{"metadata":{"message_id":"a","message_type":"notification"},"payload":{"event":{}}}"#,
        );
        assert!(env.into_incoming().is_err());
    }

    /// The full `channel.chat.message` event shape, including the nesting that matters: `text`
    /// lives inside `message`, and `bits` inside `cheer`.
    #[test]
    fn chat_message_flattens_the_nested_fields() {
        let event = serde_json::json!({
            "broadcaster_user_id": "12826",
            "broadcaster_user_login": "twitch",
            "broadcaster_user_name": "Twitch",
            "chatter_user_id": "141981764",
            "chatter_user_login": "viewer42",
            "chatter_user_name": "Viewer42",
            "message_id": "cc106a89-1814-919d-454c-f4f2f2f2f2f2",
            "message": {
                "text": "!play never gonna give you up",
                "fragments": [
                    { "type": "text", "text": "!play never gonna give you up" }
                ]
            },
            "message_type": "text",
            "badges": [
                { "set_id": "moderator", "id": "1", "info": "" },
                { "set_id": "subscriber", "id": "12", "info": "6" }
            ],
            "color": "#1E90FF",
            "cheer": { "bits": 500 }
        });
        let msg = ChatMessage::from_event(&event).unwrap();
        assert_eq!(msg.text, "!play never gonna give you up");
        assert_eq!(msg.chatter_user_login, "viewer42");
        assert_eq!(msg.bits, Some(500));
        assert_eq!(msg.reward_id, None);
        assert!(!msg.is_reply);
        assert_eq!(msg.badge("moderator").unwrap().id, "1");
        assert_eq!(msg.badge("subscriber").unwrap().info, "6");
        assert!(msg.badge("vip").is_none());
    }

    /// A plain message has no `cheer`, no `reply` and no reward. All the optional fields must
    /// degrade rather than fail the parse — the common case is the minimal one.
    #[test]
    fn a_minimal_chat_message_parses() {
        let event = serde_json::json!({
            "message_id": "m1",
            "chatter_user_id": "1",
            "chatter_user_login": "someone",
            "chatter_user_name": "Someone",
            "message": { "text": "hola", "fragments": [] }
        });
        let msg = ChatMessage::from_event(&event).unwrap();
        assert_eq!(msg.text, "hola");
        assert_eq!(msg.bits, None);
        assert_eq!(msg.reward_id, None);
        assert!(msg.badges.is_empty());
    }

    /// A redemption-sourced message carries the reward id — the field phase 4 keys on.
    #[test]
    fn reward_messages_carry_the_reward_id() {
        let event = serde_json::json!({
            "message_id": "m2",
            "chatter_user_id": "1",
            "chatter_user_login": "someone",
            "chatter_user_name": "Someone",
            "message": { "text": "darude sandstorm" },
            "channel_points_custom_reward_id": "reward-abc"
        });
        let msg = ChatMessage::from_event(&event).unwrap();
        assert_eq!(msg.reward_id.as_deref(), Some("reward-abc"));
    }

    /// A truncated event must be an error, not a message with empty fields — an empty chat line
    /// would look like a viewer sent nothing.
    #[test]
    fn an_incomplete_event_is_an_error() {
        let event = serde_json::json!({ "message_id": "m3", "message": { "text": "hi" } });
        assert!(ChatMessage::from_event(&event).is_err());
    }

    /// The serialised shape the UI reads is camelCase, while deserialisation stays on Twitch's
    /// snake_case. Renaming both would break parsing; renaming neither would put snake_case inside
    /// an otherwise camelCase payload.
    #[test]
    fn chat_message_serialises_camel_case_but_reads_snake_case() {
        let event = serde_json::json!({
            "message_id": "m4",
            "chatter_user_id": "1",
            "chatter_user_login": "someone",
            "chatter_user_name": "Someone",
            "message": { "text": "hi" },
            "badges": [ { "set_id": "moderator", "id": "1", "info": "" } ]
        });
        let msg = ChatMessage::from_event(&event).unwrap();
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"chatterUserName\":\"Someone\""), "{json}");
        assert!(json.contains("\"isReply\":false"), "{json}");
        assert!(!json.contains("chatter_user_name"));
        // A nested struct needs the same treatment, or one snake_case object ends up inside a
        // camelCase payload — which is exactly how a badge list silently stops rendering.
        assert!(json.contains("\"setId\""), "{json}");
        assert!(!json.contains("set_id"), "{json}");
        // And it round-trips back into Twitch's naming, so a stored copy would still parse.
        let back: ChatMessage = serde_json::from_str(
            r#"{"message_id":"m4","chatter_user_id":"1","chatter_user_login":"someone",
                "chatter_user_name":"Someone","text":"hi"}"#,
        )
        .unwrap();
        assert_eq!(back.text, "hi");
    }

    /// At-least-once delivery: the first sighting is accepted, every repeat is dropped, and the
    /// counter records what was dropped so the UI can prove the mechanism is live.
    #[test]
    fn dedupe_accepts_once_and_counts_repeats() {
        let mut d = Dedupe::new();
        assert!(d.accept("id-1"));
        assert!(!d.accept("id-1"));
        assert!(!d.accept("id-1"));
        assert!(d.accept("id-2"));
        assert_eq!(d.dropped(), 2);
        assert_eq!(d.len(), 2);
    }

    /// The window is bounded: a busy channel must not grow this for the life of the process. The
    /// oldest id is forgotten, so a very old duplicate counts as new — the right direction to fail.
    #[test]
    fn dedupe_window_is_bounded_and_forgets_the_oldest() {
        let mut d = Dedupe::new();
        for i in 0..(DEDUPE_WINDOW + 10) {
            assert!(d.accept(&format!("id-{i}")));
        }
        assert_eq!(d.len(), DEDUPE_WINDOW);
        // Long evicted: treated as new rather than silently swallowed.
        assert!(d.accept("id-0"));
        // Still inside the window: suppressed.
        assert!(!d.accept(&format!("id-{}", DEDUPE_WINDOW + 9)));
    }

    /// Changing channel must not carry ids across. The app achieves that by dropping the whole
    /// `Dedupe` — `events_loop` makes one per loop, and a channel change bumps the generation — so
    /// this asserts the property that makes that sufficient: a fresh window accepts everything.
    #[test]
    fn a_fresh_dedupe_accepts_ids_the_old_one_had_seen() {
        let mut first = Dedupe::new();
        first.accept("id-1");
        let mut second = Dedupe::new();
        assert!(second.accept("id-1"), "a new channel must not inherit the old window");
        assert_eq!(second.dropped(), 0);
    }

    /// The close codes are the difference between "retry" and "you are doing it wrong", so they are
    /// spelled out rather than shown as a number.
    #[test]
    fn close_codes_are_explained() {
        use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
        let frame = |code: u16| Some(CloseFrame { code: CloseCode::from(code), reason: "".into() });
        assert!(close_reason(frame(4003)).contains("never used"));
        assert!(close_reason(frame(4002)).contains("pong"));
        assert!(close_reason(frame(4004)).contains("reconnect"));
        assert!(close_reason(None).contains("closed"));
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(6), Duration::from_secs(32));
        // Capped, not overflowing: a long outage must not produce a 2^40-second sleep.
        assert_eq!(backoff_delay(99), Duration::from_secs(32));
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
    }

    #[test]
    fn no_addresses_is_an_error_not_a_hang() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        assert!(rt.block_on(connect_first(&[])).is_err());
    }
}
