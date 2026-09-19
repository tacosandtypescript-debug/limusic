//! Twitch integration — phase 1: the account session.
//!
//! This module is a **sidecar**. It owns the Twitch connection and nothing else: no access to the
//! queue, no access to libmpv, and no reference back to `AppState`. When phase 3 needs Twitch to
//! move the music, it will do what Listen Together already does — send a command down an
//! `mpsc` channel that a bridge in `lib.rs` applies — so the reproduction path keeps exactly one
//! owner. That is also why nothing here is wired into `AppState` yet: phase 1 has no command to
//! send, and an empty bridge is dead code.
//!
//! The shape is copied from `listentogether/mod.rs` on purpose, because that module already solved
//! the problems this one has (a connection that can be cancelled, state shared with the UI,
//! reconnection without leaking tasks):
//!
//! ```text
//! TwitchSession { app, db, inner: Arc<Mutex<Inner>>, gen: AtomicU64 }
//! ```
//!
//! `gen` is the cancellation token. Every connect bumps it; a task compares the value it captured
//! against the current one and exits if they differ. That is how a second connect attempt, a
//! cancel, or a disconnect stops an in-flight device-flow poll without any `JoinHandle`
//! bookkeeping — the same trick `LtSession` uses, and the same one `lastfm.rs` calls `auth_gen`.
//!
//! Two Twitch requirements are implemented here rather than left to the caller:
//!
//! * **`/validate` at startup and hourly.** This is a licence condition, not a nicety: *"Any
//!   third-party app that calls the Twitch APIs and maintains an OAuth session must call the
//!   /validate endpoint… when it starts and on an hourly basis thereafter"*, and Twitch audits it.
//! * **Single-use refresh tokens.** Every refresh returns a replacement and invalidates the old,
//!   so the write-back is not optional. See [`auth::TokenSet::merge_refresh`].

mod api;
mod auth;
mod events;
mod settings;

/// Public so `lib.rs` can register the handlers as `twitch::commands::tw_*`, mirroring how the
/// app's own commands are registered. `#[tauri::command]` generates a companion macro next to the
/// function, and `generate_handler!` resolves it through that same module path — so this must be
/// `pub` rather than re-exported with a glob.
pub mod commands;

// Phase 3: who may ask, how often, and what they may ask for.
//
// Every one of these is pure — badge ids, instants and message text in, an answer out — and every
// one has tests in `tools/verify`. That is deliberate: this file is the only part that needs a
// socket and an app handle, and it is the part that cannot be tested here, so none of the
// *decisions* are allowed to live in it. What is left is orchestration.
//
// The bridge these feed is in `lib.rs`: an `mpsc` channel this module sends down and the app applies
// to `AppState`, so the playback path keeps one owner.
//
// `dead_code` stays allowed, for a reason that is not "later": each of these exposes a small API
// that its *tests* exercise, and the tests live in another crate (`tools/verify`), so from the app's
// point of view a function only the tests call has no caller at all. The alternative is deleting
// behaviour that is asserted to work — `Role::label` is the inverse of `Role::parse`, `pick` is the
// policy for choosing between search results — to satisfy a linter that cannot see the caller.
#[allow(dead_code)]
pub mod chat;
#[allow(dead_code)]
pub mod cooldown;
#[allow(dead_code)]
pub mod permissions;
#[allow(dead_code)]
pub mod requests;
#[allow(dead_code)]
pub mod rewards;

pub use api::{Helix, HelixError, TwitchUser};
pub use settings::TwitchConfig;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Mutex};

use crate::db::Db;
use auth::{DeviceCode, Poll, RefreshError, TokenSet};
use events::{ChatMessage, Dedupe, Incoming};
use settings::{
    KEY_ACCESS_TOKEN, KEY_CLIENT_ID, KEY_CONFIG, KEY_EXPIRES_AT, KEY_REFRESH_TOKEN, KEY_SCOPES,
    KEY_VALIDATED_AT,
};

/// Emitted whenever anything about the session changes. The payload is [`Snapshot`].
pub const EVENT_STATE: &str = "tw-state";

/// How often `/validate` runs once connected. One hour is what Twitch asks for; the small margin
/// below the hour is irrelevant, but running *more* often is not free (it is a request against the
/// rate-limit bucket), so this is not a "just in case" shorter interval.
const VALIDATE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Treat a token as stale this many seconds before its recorded expiry, so a request cannot be
/// issued with a token that dies mid-flight.
const EXPIRY_SKEW: i64 = 60;

/// Where the session stands. Serialised lowercase for the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// No token, or the stored one was rejected.
    Disconnected,
    /// A device flow is in flight and waiting for the user to approve it.
    Connecting,
    /// A valid token is held.
    Connected,
}

/// The device-flow prompt, handed to the UI so it can show the code and open the browser.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DevicePrompt {
    pub user_code: String,
    pub verification_uri: String,
    /// Unix seconds; past this the code is dead and the flow must be restarted.
    pub expires_at: i64,
}

/// What the UI is told. Deliberately a hand-built struct rather than the whole `Inner`: adding a
/// secret field to the session must not be able to leak it by accident.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub phase: Phase,
    /// The effective client ID, and whether it came from the build rather than from the user.
    pub client_id: String,
    pub client_id_bundled: bool,
    /// Whether a client ID is set at all — the one thing that must be true before connecting.
    pub configured: bool,
    pub device: Option<DevicePrompt>,
    pub account: Option<TwitchUser>,
    pub channel: Option<TwitchUser>,
    /// Whether the selected channel may own Channel Points rewards at all.
    ///
    /// Twitch 403s those endpoints for anyone who is not an affiliate or partner ("The broadcaster
    /// is not a partner or affiliate."), which is not obvious from the message and is not
    /// retryable. Surfacing it here means the panel can say so before the streamer configures
    /// rewards that could never be created.
    pub channel_points_available: bool,
    pub config: TwitchConfig,
    pub scopes: Vec<String>,
    pub expires_at: i64,
    pub validated_at: i64,
    pub error: Option<String>,
    /// Whether chat is being read, and what has come through.
    pub eventsub: EventSubSnapshot,
}

/// How many recent chat messages to keep for the panel's tail. Small on purpose: this rides the
/// same snapshot that every state change emits, and a chat tail is a debugging aid, not a log.
const TAIL_LEN: usize = 15;

/// Where the EventSub connection stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventSubStatus {
    /// No connection, and none wanted — not connected, or no channel chosen.
    #[default]
    Off,
    /// Opening the socket, or waiting for its welcome.
    Connecting,
    /// Subscribed and receiving.
    Live,
    /// Dropped, waiting out the backoff before trying again.
    Retrying,
    /// Gave up, or was refused in a way retrying cannot fix (a 403, a revoked subscription).
    Failed,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSubSnapshot {
    pub status: EventSubStatus,
    /// The session id subscriptions are bound to. Shown truncated in the panel: it is the single
    /// value that says "this is a real session and not a stalled connect".
    pub session_id: Option<String>,
    pub subscription_id: Option<String>,
    /// Unix seconds when the current session went live.
    pub connected_at: i64,
    /// Notifications accepted since the session started.
    pub messages: u64,
    /// Notifications dropped as redeliveries. Shown so that "dedupe works" is observable rather
    /// than assumed.
    pub duplicates: u64,
    pub error: Option<String>,
    /// The tail, newest first.
    pub recent: Vec<ChatMessage>,
}

/// What the session asks the app to do with the music.
///
/// The session owns the Twitch connection and nothing else — see the module header. It does not
/// touch the queue and holds no reference to `AppState`, so a request travels down a channel whose
/// other end `lib.rs` owns and applies. That is the same shape Listen Together uses, and it is what
/// keeps exactly one owner of the playback path.
///
/// The `oneshot` is what makes an answer possible. The bridge is one-way, so without a way back the
/// session would fire a request into the dark and have nothing to tell the viewer — and a viewer who
/// is not told asks again, which is the whole reason the cooldown exists.
pub enum TwitchCommand {
    Request {
        query: String,
        /// Who asked, for the queue entry's label: `twitch:<login>`.
        from: String,
        /// Where the outcome comes back. Dropped without a word if the app is shutting down, which
        /// is why the caller treats a closed channel as "no answer" rather than as a failure.
        reply: tokio::sync::oneshot::Sender<requests::Outcome>,
    },
}

struct Inner {
    config: TwitchConfig,
    client_id: String,
    phase: Phase,
    account: Option<TwitchUser>,
    channel: Option<TwitchUser>,
    device: Option<DevicePrompt>,
    scopes: Vec<String>,
    expires_at: i64,
    validated_at: i64,
    error: Option<String>,
    /// The account the access token belongs to, from `/validate`. The `user_id` half of an
    /// EventSub chat condition.
    token_user_id: Option<String>,
    eventsub: EventSub,
}

/// The EventSub side of the session.
#[derive(Default)]
struct EventSub {
    status: EventSubStatus,
    session_id: Option<String>,
    subscription_id: Option<String>,
    connected_at: i64,
    messages: u64,
    duplicates: u64,
    error: Option<String>,
    /// Newest first, so the panel renders it in order without reversing.
    recent: VecDeque<ChatMessage>,
}

impl EventSub {
    fn snapshot(&self) -> EventSubSnapshot {
        EventSubSnapshot {
            status: self.status,
            session_id: self.session_id.clone(),
            subscription_id: self.subscription_id.clone(),
            connected_at: self.connected_at,
            messages: self.messages,
            duplicates: self.duplicates,
            error: self.error.clone(),
            recent: self.recent.iter().cloned().collect(),
        }
    }

    /// A fresh session: everything tied to the old socket goes, because a stale session id or
    /// subscription id would describe a connection that no longer exists.
    fn reset(&mut self) {
        self.session_id = None;
        self.subscription_id = None;
        self.connected_at = 0;
        self.error = None;
        self.recent.clear();
    }
}

/// An EventSub failure, carrying whether trying again could help.
///
/// This distinction is the whole reason the error type exists. A dropped socket, a timeout and a
/// failed handshake are worth retrying; "no channel chosen", "we do not know which account this
/// token is" and a 403 from Twitch are not — retrying those forever would hammer the API with a
/// request that cannot succeed until the user changes something.
struct EventSubError {
    message: String,
    retryable: bool,
}

impl EventSubError {
    fn retryable(message: impl Into<String>) -> Self {
        EventSubError { message: message.into(), retryable: true }
    }

    fn fatal(message: impl Into<String>) -> Self {
        EventSubError { message: message.into(), retryable: false }
    }
}

impl std::fmt::Display for EventSubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub struct TwitchSession {
    app: AppHandle,
    db: Arc<Db>,
    inner: Arc<Mutex<Inner>>,
    /// Cancellation token for every in-flight task. See the module docs.
    gen: AtomicU64,
    /// Cancellation for the EventSub loop alone. Separate from `gen` because the two have
    /// different lifetimes: the OAuth session outlives any particular socket, and changing channel
    /// restarts the socket without touching the session.
    events_gen: AtomicU64,
    /// Outbound requests to the music side. `lib.rs` owns the receiving end.
    commands: mpsc::UnboundedSender<TwitchCommand>,
    /// The receiving end, held until `lib.rs` takes it at startup.
    command_rx: Mutex<Option<mpsc::UnboundedReceiver<TwitchCommand>>>,
    /// The cooldown windows and who asked last. Behind a mutex because it is the state that makes
    /// the feature work, and shared because two requests can arrive at once.
    cooldowns: Mutex<cooldown::Cooldowns>,
    /// The windows `cooldowns` was built with, so a settings change can rebuild it.
    cooldown_config: Mutex<(u64, u64)>,
    /// How many requests have been answered, and how many of them queued something. In the snapshot
    /// so the panel can show that the feature is alive rather than merely enabled.
    answered: AtomicU64,
    queued: AtomicU64,
}

impl TwitchSession {
    /// Build the session from whatever is stored. Does no I/O beyond reading settings, so it is
    /// safe to call on the startup path before the window exists.
    pub fn new(app: AppHandle, db: Arc<Db>) -> Arc<Self> {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let config = TwitchConfig::from_json(db.get_setting(KEY_CONFIG).as_deref());
        let stored_client_id = db.get_setting(KEY_CLIENT_ID).unwrap_or_default();
        let bundled = settings::bundled_client_id();
        // A stored value wins over the bundled one: someone who pasted their own client ID into
        // Settings meant it, and silently overriding it with a build-time default would be
        // baffling.
        let client_id = if stored_client_id.trim().is_empty() {
            bundled.trim().to_owned()
        } else {
            stored_client_id
        };
        // Read before `config` moves into `Inner`: the cooldown windows seed the shared state, and
        // a field cannot be read out of a struct that has already been given away.
        let (user_cd, global_cd) = (config.user_cooldown_secs, config.global_cooldown_secs);
        Arc::new(TwitchSession {
            app,
            db,
            inner: Arc::new(Mutex::new(Inner {
                config,
                client_id,
                phase: Phase::Disconnected,
                account: None,
                channel: None,
                device: None,
                scopes: Vec::new(),
                expires_at: 0,
                validated_at: 0,
                error: None,
                token_user_id: None,
                eventsub: EventSub::default(),
            })),
            gen: AtomicU64::new(0),
            events_gen: AtomicU64::new(0),
            commands,
            command_rx: Mutex::new(Some(command_rx)),
            cooldowns: Mutex::new(cooldown::Cooldowns::new(
                Duration::from_secs(user_cd),
                Duration::from_secs(global_cd),
            )),
            cooldown_config: Mutex::new((user_cd, global_cd)),
            answered: AtomicU64::new(0),
            queued: AtomicU64::new(0),
        })
    }

    /// Hand the receiving end to `lib.rs`, once, at startup.
    ///
    /// `Option` rather than a bare receiver so a second call is harmless: a future hot-reload or a
    /// second window would otherwise either panic or steal the channel, and the bridge would stop
    /// without saying so.
    pub async fn take_commands(&self) -> Option<mpsc::UnboundedReceiver<TwitchCommand>> {
        self.command_rx.lock().await.take()
    }

    /// Rebuild the cooldowns when the configured windows have changed.
    ///
    /// Rebuilding forgets who asked when, and that is the right trade: someone who has just widened
    /// or closed a window is not owed the old one, and keeping the windows in two places would mean
    /// deciding which of them is authoritative on every request.
    async fn cooldowns(
        &self,
        user_secs: u64,
        global_secs: u64,
    ) -> tokio::sync::MutexGuard<'_, cooldown::Cooldowns> {
        {
            let mut current = self.cooldown_config.lock().await;
            if *current != (user_secs, global_secs) {
                *current = (user_secs, global_secs);
                *self.cooldowns.lock().await = cooldown::Cooldowns::new(
                    Duration::from_secs(user_secs),
                    Duration::from_secs(global_secs),
                );
            }
        }
        let mut guard = self.cooldowns.lock().await;
        // Dead entries cannot change an answer, and a busy channel accumulates one per viewer.
        guard.prune(std::time::Instant::now());
        guard
    }

    // --- tokens ------------------------------------------------------------------------------

    fn load_tokens(&self) -> Option<TokenSet> {
        let access_token = self.db.get_setting(KEY_ACCESS_TOKEN).filter(|s| !s.is_empty())?;
        Some(TokenSet {
            access_token,
            refresh_token: self.db.get_setting(KEY_REFRESH_TOKEN).filter(|s| !s.is_empty()),
            scopes: self
                .db
                .get_setting(KEY_SCOPES)
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            expires_at: self
                .db
                .get_setting(KEY_EXPIRES_AT)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        })
    }

    /// Persist a token set. Called after **every** refresh, not just the first grant: the refresh
    /// token is single-use, so a token we failed to write back is a token that no longer works.
    fn save_tokens(&self, tokens: &TokenSet) {
        self.db.set_setting(KEY_ACCESS_TOKEN, &tokens.access_token);
        self.db.set_setting(KEY_REFRESH_TOKEN, tokens.refresh_token.as_deref().unwrap_or(""));
        self.db.set_setting(KEY_SCOPES, &tokens.scopes.join(" "));
        self.db.set_setting(KEY_EXPIRES_AT, &tokens.expires_at.to_string());
    }

    fn clear_tokens(&self) {
        for key in [KEY_ACCESS_TOKEN, KEY_REFRESH_TOKEN, KEY_SCOPES, KEY_EXPIRES_AT] {
            self.db.delete_setting(key);
        }
    }

    // --- state ------------------------------------------------------------------------------

    fn http(&self) -> &'static reqwest::Client {
        crate::http::client()
    }

    pub async fn snapshot(&self) -> Snapshot {
        let inner = self.inner.lock().await;
        Snapshot {
            phase: inner.phase,
            client_id: inner.client_id.clone(),
            client_id_bundled: inner.client_id == settings::bundled_client_id().trim(),
            configured: !inner.client_id.trim().is_empty(),
            device: inner.device.clone(),
            account: inner.account.clone(),
            channel: inner.channel.clone(),
            channel_points_available: inner
                .channel
                .as_ref()
                .is_some_and(TwitchUser::can_use_channel_points),
            config: inner.config.clone(),
            scopes: inner.scopes.clone(),
            expires_at: inner.expires_at,
            validated_at: inner.validated_at,
            error: inner.error.clone(),
            eventsub: inner.eventsub.snapshot(),
        }
    }

    /// Recompute the public state and push it to the UI. Every mutation funnels through here, so
    /// the renderer cannot be shown a state the session is not in.
    async fn emit(&self) {
        let snapshot = self.snapshot().await;
        let _ = self.app.emit(EVENT_STATE, snapshot);
    }

    async fn fail(&self, error: impl Into<String>) {
        {
            let mut inner = self.inner.lock().await;
            inner.error = Some(error.into());
            inner.phase = Phase::Disconnected;
            inner.device = None;
        }
        self.emit().await;
    }

    // --- lifecycle --------------------------------------------------------------------------

    /// Restore a stored session and start the hourly validation loop.
    ///
    /// Runs on every launch when a token exists — even with `auto_connect` off — because Twitch
    /// requires validation at startup from any app that maintains a session, and because a token
    /// that has been revoked should be visible as "disconnected" rather than discovered later by a
    /// mysterious 401.
    pub fn restore(self: &Arc<Self>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            if me.load_tokens().is_some() {
                me.validate_stored().await;
            }
            me.maintenance_loop().await;
        });
    }

    /// Validate the stored token, refreshing first if it looks expired.
    async fn validate_stored(self: &Arc<Self>) {
        let Some(tokens) = self.load_tokens() else { return };
        let client_id = { self.inner.lock().await.client_id.clone() };
        if client_id.trim().is_empty() {
            return;
        }

        let now = crate::db::now_secs();
        let tokens = if tokens.is_expired(now, EXPIRY_SKEW) {
            match self.try_refresh(&client_id, &tokens).await {
                Ok(fresh) => fresh,
                Err(e) => return self.fail(e).await,
            }
        } else {
            tokens
        };

        match auth::validate(self.http(), &tokens.access_token).await {
            Ok(v) => self.accept_session(&client_id, tokens, v).await,
            Err(message) => {
                // A 401 from /validate is the documented refresh trigger.
                if message.to_ascii_lowercase().contains("invalid access token") {
                    match self.try_refresh(&client_id, &tokens).await {
                        Ok(fresh) => match auth::validate(self.http(), &fresh.access_token).await {
                            Ok(v) => self.accept_session(&client_id, fresh, v).await,
                            Err(m) => self.fail(format!("Twitch rejected the session: {m}")).await,
                        },
                        Err(e) => self.fail(e).await,
                    }
                } else {
                    // A network failure must not throw away a working token.
                    self.fail(format!("Could not validate the Twitch session: {message}")).await;
                }
            }
        }
    }

    /// Store the outcome of a successful validation: who we are, which channel, until when.
    async fn accept_session(
        self: &Arc<Self>,
        client_id: &str,
        tokens: TokenSet,
        validated: auth::Validated,
    ) {
        self.save_tokens(&tokens);
        self.db.set_setting(KEY_VALIDATED_AT, &crate::db::now_secs().to_string());

        // Resolve the account name for the UI. Best effort: a failure here does not invalidate the
        // session, and the panel can show the login from /validate instead.
        let account =
            match Helix::new(self.http(), client_id, &tokens.access_token).current_user().await {
                Ok(user) => user,
                Err(e) => {
                    tracing::debug!(error = %e, "twitch: could not resolve the account name");
                    None
                }
            };

        let mut inner = self.inner.lock().await;
        inner.client_id = client_id.to_owned();
        inner.phase = Phase::Connected;
        inner.device = None;
        inner.error = None;
        inner.scopes =
            if validated.scopes.is_empty() { tokens.scopes.clone() } else { validated.scopes };
        inner.expires_at = tokens.expires_at;
        inner.validated_at = crate::db::now_secs();
        inner.account = account;
        // `/validate` is the authoritative answer to "who is this token", and an EventSub chat
        // condition needs exactly that id ("The User ID to read chat as"). Taking it from here
        // rather than from the `/helix/users` call above means the subscription still works when
        // that best-effort name lookup fails.
        inner.token_user_id = validated.user_id.clone();
        drop(inner);

        // The configured channel may have been chosen in an earlier session; re-resolve it so the
        // panel reflects what is really stored rather than a stale copy.
        self.refresh_channel().await;
        self.emit().await;
        tracing::info!("twitch: session validated");

        // Now that the account and channel are known, start reading chat. A no-op when no channel
        // has been chosen yet — `set_channel` starts it in that case.
        self.start_events_if_ready().await;
    }

    /// Try a refresh, mapping the two failure kinds onto what the user should do about it.
    async fn try_refresh(&self, client_id: &str, tokens: &TokenSet) -> Result<TokenSet, String> {
        let Some(refresh_token) = tokens.refresh_token.as_deref().filter(|t| !t.is_empty()) else {
            return Err("Twitch session expired. Connect again.".into());
        };
        match auth::refresh(self.http(), client_id, refresh_token).await {
            Ok(fresh) => {
                // Write immediately: the token we just spent is already dead server-side, so a
                // crash between here and the caller would leave an unusable row on disk.
                let mut merged = tokens.clone();
                merged.merge_refresh(fresh);
                self.save_tokens(&merged);
                Ok(merged)
            }
            Err(RefreshError::Rejected(message)) => {
                // The grant is gone (30 days unused, revoked, or password changed). Keeping the
                // dead row would make the next launch retry a token that can never work.
                self.clear_tokens();
                Err(format!("Twitch sign-in expired ({message}). Connect again."))
            }
            Err(RefreshError::Transient(message)) => {
                Err(format!("Could not reach Twitch to refresh the session: {message}"))
            }
        }
    }

    /// A valid access token, refreshing if needed. This is the entry point later phases use before
    /// any Helix call, so the "is it fresh" decision lives in exactly one place.
    pub async fn access_token(self: &Arc<Self>) -> Result<String, String> {
        let tokens = self.load_tokens().ok_or("Not connected to Twitch.")?;
        let client_id = { self.inner.lock().await.client_id.clone() };
        if tokens.is_expired(crate::db::now_secs(), EXPIRY_SKEW) {
            return Ok(self.try_refresh(&client_id, &tokens).await?.access_token);
        }
        Ok(tokens.access_token)
    }

    /// The hourly `/validate`, plus a proactive refresh just before the token expires so an active
    /// session does not depend on hitting a 401 first.
    async fn maintenance_loop(self: &Arc<Self>) {
        loop {
            tokio::time::sleep(VALIDATE_EVERY).await;
            let (connected, client_id) = {
                let inner = self.inner.lock().await;
                (inner.phase == Phase::Connected, inner.client_id.clone())
            };
            if !connected || client_id.trim().is_empty() {
                continue;
            }
            if let Ok(token) = self.access_token().await {
                match auth::validate(self.http(), &token).await {
                    Ok(v) => {
                        let mut inner = self.inner.lock().await;
                        inner.validated_at = crate::db::now_secs();
                        inner.scopes = v.scopes;
                        drop(inner);
                        // Persist so an offline audit trail exists in the log, which is the only
                        // evidence a "we validate hourly" claim has.
                        self.db.set_setting(KEY_VALIDATED_AT, &crate::db::now_secs().to_string());
                        tracing::debug!("twitch: hourly token validation ok");
                    }
                    Err(message) => {
                        // A refresh has already been attempted by `access_token`, so a failure
                        // here means the session is genuinely gone.
                        tracing::warn!(error = %message, "twitch: hourly validation failed");
                        self.clear_tokens();
                        self.fail(format!("Twitch session is no longer valid: {message}")).await;
                    }
                }
            }
        }
    }

    // --- EventSub ---------------------------------------------------------------------------
    //
    // One socket carries every subscription, so this section owns a loop, a session and a
    // backoff, and the protocol itself lives in `events.rs`.

    /// Start reading chat, if there is anything to read. Harmless to call repeatedly: each call
    /// supersedes the last, so it doubles as "restart".
    async fn start_events_if_ready(self: &Arc<Self>) {
        let ready = {
            let inner = self.inner.lock().await;
            inner.phase == Phase::Connected
                && inner.config.channel_id.is_some()
                && inner.token_user_id.is_some()
        };
        if !ready {
            return;
        }
        self.events_gen.fetch_add(1, Ordering::SeqCst);
        let my_gen = self.events_gen.load(Ordering::SeqCst);
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            me.events_loop(my_gen).await;
        });
    }

    /// Stop reading chat and forget the session that was doing it.
    async fn stop_events(self: &Arc<Self>) {
        self.events_gen.fetch_add(1, Ordering::SeqCst);
        {
            let mut inner = self.inner.lock().await;
            inner.eventsub.reset();
            inner.eventsub.status = EventSubStatus::Off;
        }
        self.emit().await;
    }

    /// The reconnect loop. Owns one generation and exits as soon as it moves.
    async fn events_loop(self: Arc<Self>, my_gen: u64) {
        let mut dedupe = Dedupe::new();
        let mut attempt: u32 = 0;

        loop {
            if self.events_gen.load(Ordering::SeqCst) != my_gen {
                return;
            }
            {
                let mut inner = self.inner.lock().await;
                inner.eventsub.status = if attempt == 0 {
                    EventSubStatus::Connecting
                } else {
                    EventSubStatus::Retrying
                };
            }
            self.emit().await;

            match self.events_session(my_gen, &mut dedupe).await {
                Ok(()) => {
                    // The socket ended without an error: a server close we should reconnect from.
                    if self.events_gen.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    attempt += 1;
                }
                Err(e) => {
                    if self.events_gen.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    // A refusal that retrying cannot fix (no channel, a 403 from a ban or the join
                    // limit, a revoked subscription) stops the loop instead of hammering Twitch
                    // forever with a request that will never succeed.
                    if !e.retryable {
                        tracing::warn!(error = %e.message, "twitch: EventSub stopped");
                        let mut inner = self.inner.lock().await;
                        inner.eventsub.status = EventSubStatus::Failed;
                        inner.eventsub.error = Some(e.message);
                        drop(inner);
                        self.emit().await;
                        return;
                    }
                    tracing::warn!(error = %e.message, "twitch: EventSub dropped, will retry");
                    attempt += 1;
                    {
                        let mut inner = self.inner.lock().await;
                        inner.eventsub.error = Some(e.message);
                        inner.eventsub.session_id = None;
                        inner.eventsub.subscription_id = None;
                    }
                }
            }

            if attempt > 15 {
                let mut inner = self.inner.lock().await;
                inner.eventsub.status = EventSubStatus::Failed;
                inner.eventsub.error = Some("Lost the EventSub connection.".into());
                drop(inner);
                self.emit().await;
                return;
            }
            tokio::time::sleep(events::backoff_delay(attempt)).await;
        }
    }

    /// One socket's lifetime: open it, establish the session, then read until it ends.
    ///
    /// `Ok(())` means it ended in a way worth retrying; `Err` carries whether it is worth retrying
    /// at all.
    async fn events_session(
        self: &Arc<Self>,
        my_gen: u64,
        dedupe: &mut Dedupe,
    ) -> Result<(), EventSubError> {
        let mut ws = events::open(events::DEFAULT_URL).await.map_err(EventSubError::retryable)?;
        self.establish(&mut ws, my_gen).await?;

        loop {
            if self.events_gen.load(Ordering::SeqCst) != my_gen {
                return Ok(());
            }
            // The 500ms tick is the cancellation poll: a quiet channel would otherwise park in
            // `next()` and never notice a disconnect or a channel change. Same shape as the
            // Listen Together loop.
            let next = tokio::select! {
                biased;
                n = events::next(&mut ws) => n,
                _ = tokio::time::sleep(Duration::from_millis(500)) => continue,
            };

            match next {
                // A second welcome on the same socket should not happen; ignoring it is safer than
                // re-subscribing, which would double every message.
                Some(Ok(Incoming::Welcome(_))) => continue,
                Some(Ok(Incoming::Keepalive)) => continue,
                Some(Ok(Incoming::Notification(n))) => self.on_notification(n, dedupe).await,
                Some(Ok(Incoming::Reconnect { url })) => {
                    // Twitch gives 30s of warning and says to use the URL as-is. The documented
                    // handover is to have the replacement *established* before dropping the socket
                    // we are on, so no events fall in the gap — hence the nested attempt rather
                    // than simply breaking out to reconnect.
                    match events::open(&url).await {
                        Ok(mut fresh) => match self.establish(&mut fresh, my_gen).await {
                            Ok(()) => {
                                ws = fresh;
                                tracing::info!("twitch: EventSub handover complete");
                            }
                            Err(e) => tracing::warn!(
                                error = %e.message,
                                "twitch: handover failed, staying on the current socket"
                            ),
                        },
                        Err(e) => tracing::warn!(
                            error = %e,
                            "twitch: could not open the reconnect URL, staying on the current socket"
                        ),
                    }
                }
                Some(Ok(Incoming::Revoked { status, kind })) => {
                    return Err(EventSubError::fatal(format!(
                        "Twitch revoked the {kind} subscription ({status})."
                    )));
                }
                Some(Ok(Incoming::Unknown { message_type })) => {
                    tracing::debug!(message_type, "twitch: ignoring an unknown EventSub frame");
                }
                Some(Err(e)) => return Err(EventSubError::retryable(e)),
                // The stream ended: reconnect rather than treat it as terminal.
                None => return Ok(()),
            }
        }
    }

    /// Read until `session_welcome`, then create the subscription.
    ///
    /// The 10-second budget is Twitch's, not ours: a session that has not been used by then is
    /// closed with 4003. So this fails fast instead of waiting on a socket that is already doomed.
    async fn establish(
        self: &Arc<Self>,
        ws: &mut events::Socket,
        my_gen: u64,
    ) -> Result<(), EventSubError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let welcome = loop {
            let next = tokio::time::timeout_at(deadline, events::next(ws))
                .await
                .map_err(|_| EventSubError::retryable("EventSub sent no welcome within 10s"))?;
            match next {
                Some(Ok(Incoming::Welcome(w))) => break w,
                // Keepalives first is legal; keep reading until the welcome.
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(EventSubError::retryable(e)),
                None => {
                    return Err(EventSubError::retryable(
                        "EventSub closed before sending a welcome",
                    ))
                }
            }
        };

        if self.events_gen.load(Ordering::SeqCst) != my_gen {
            return Ok(());
        }
        {
            let mut inner = self.inner.lock().await;
            inner.eventsub.reset();
            inner.eventsub.session_id = Some(welcome.session_id.clone());
            inner.eventsub.status = EventSubStatus::Live;
            inner.eventsub.connected_at = crate::db::now_secs();
        }

        let subscription = self.subscribe_all(&welcome.session_id).await?;
        {
            let mut inner = self.inner.lock().await;
            inner.eventsub.subscription_id = Some(subscription.id);
            inner.eventsub.error = None;
        }
        self.emit().await;
        tracing::info!(
            keepalive = welcome.keepalive_timeout_seconds,
            cost = subscription.cost,
            "twitch: EventSub live"
        );
        Ok(())
    }

    /// Create every subscription this session needs, bound to this socket's session.
    ///
    /// Chat always; the redemption one only when a reward has been chosen. The EventSub budget for
    /// a WebSocket session is **10**, so the second subscription is not free — but it is one, and a
    /// channel that has not configured a reward should not be spending it on redemptions nobody
    /// will act on.
    ///
    /// Returns the chat subscription, which is the one the snapshot reports. A failure on the
    /// redemption one is not fatal: it is logged and the session keeps reading chat, because losing
    /// the reward feature is a smaller thing than losing the connection.
    async fn subscribe_all(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<api::Subscription, EventSubError> {
        let subscription = self.subscribe_chat(session_id).await?;
        let reward = {
            let inner = self.inner.lock().await;
            inner.config.reward_id.trim().to_string()
        };
        if !reward.is_empty() {
            if let Err(e) = self.subscribe_redemptions(session_id, &reward).await {
                tracing::warn!(error = %e, "twitch: redemptions are not subscribed; chat still is");
            }
        }
        Ok(subscription)
    }

    /// Create the `channel.chat.message` subscription bound to this socket's session.
    async fn subscribe_chat(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Result<api::Subscription, EventSubError> {
        let token = self.access_token().await.map_err(EventSubError::retryable)?;
        let (client_id, broadcaster_id, user_id) = {
            let inner = self.inner.lock().await;
            let broadcaster = inner
                .config
                .channel_id
                .clone()
                .ok_or_else(|| EventSubError::fatal("Choose a channel first."))?;
            // Without this the condition cannot be built at all, and guessing would subscribe to
            // the wrong user's view of chat.
            let user = inner.token_user_id.clone().ok_or_else(|| {
                EventSubError::fatal("Twitch did not report which account this token belongs to.")
            })?;
            (inner.client_id.clone(), broadcaster, user)
        };

        api::Helix::new(self.http(), &client_id, &token)
            .subscribe_chat(session_id, &broadcaster_id, &user_id)
            .await
            .map_err(|e| {
                // 403 on this endpoint is documented as either a ban/timeout in the channel or
                // having hit the concurrent-join limit, and it says the same thing for both
                // ("subscription missing proper authorization"). Retrying fixes neither.
                let message = self.explain(e);
                EventSubError::fatal(message)
            })
    }

    /// Create the redemption subscription for one reward.
    ///
    /// Unlike chat, the condition names no user: a redemption is the broadcaster's to see, so there
    /// is nobody to read it as. It does need the channel, resolved the same way and failing the same
    /// way, because subscribing without one would attach the subscription to nothing.
    async fn subscribe_redemptions(
        self: &Arc<Self>,
        session_id: &str,
        reward_id: &str,
    ) -> Result<api::Subscription, EventSubError> {
        let token = self.access_token().await.map_err(EventSubError::retryable)?;
        let (client_id, broadcaster_id) = {
            let inner = self.inner.lock().await;
            let broadcaster = inner
                .config
                .channel_id
                .clone()
                .ok_or_else(|| EventSubError::fatal("Choose a channel first."))?;
            (inner.client_id.clone(), broadcaster)
        };

        api::Helix::new(self.http(), &client_id, &token)
            .subscribe_redemptions(session_id, &broadcaster_id, reward_id)
            .await
            .map_err(|e| EventSubError::fatal(self.explain(e)))
    }

    /// Handle one notification: dedupe, parse, and put it in the tail.
    async fn on_notification(self: &Arc<Self>, n: events::Notification, dedupe: &mut Dedupe) {
        // Phase 3 subscribes to two types. A third means a subscription this app did not create
        // is bound to this session, which the dedupe below would otherwise let through.
        if n.kind == rewards::REDEMPTION_ADD {
            // The dedupe key for a redemption is its own id, which Twitch repeats across the
            // follow-up `update` events for the same redemption.
            if dedupe.accept(&format!("redemption:{}", n.message_id)) {
                self.on_redemption(&n.event).await;
            }
            return;
        }
        if n.kind != events::CHAT_MESSAGE {
            tracing::debug!(kind = %n.kind, "twitch: notification for an unsubscribed type");
            return;
        }
        // Twitch delivers *at least once*: a redelivery carries the same `message_id`. Parse
        // before deciding, so an unparseable event does not consume a dedupe slot forever.
        let message = match ChatMessage::from_event(&n.event) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(error = %e, "twitch: unparseable chat message");
                return;
            }
        };
        let fresh = dedupe.accept(&n.message_id);

        {
            let mut inner = self.inner.lock().await;
            // `dedupe` owns the count; mirroring it here keeps one source of truth while still
            // putting the number in the snapshot.
            inner.eventsub.duplicates = dedupe.dropped();
            if fresh {
                inner.eventsub.messages += 1;
                // A clone, because the tail is a display buffer and the pipeline below wants the
                // message itself. Both only happen for a `fresh` delivery, so a redelivery costs
                // nothing — and a chat message is a few hundred bytes.
                inner.eventsub.recent.push_front(message.clone());
                while inner.eventsub.recent.len() > TAIL_LEN {
                    inner.eventsub.recent.pop_back();
                }
            } else {
                tracing::debug!(user = %message.chatter_user_login, "twitch: duplicate chat message dropped");
            }
        }
        self.emit().await;

        // After the emit, and only for a message Twitch delivered once. Answering before the tail
        // is updated would put the reply in the panel before the message it answers.
        if fresh {
            self.on_chat_message(message).await;
        }
    }

    // --- answering the channel (phase 3) -------------------------------------------------------

    /// Run one request through the whole thing and say what happened.
    ///
    /// `badge_sets` is empty when the request came from a redemption, and `from_reward` says so: a
    /// redemption has no badges because Twitch decides who may redeem, through the reward's own
    /// settings. The spend *is* the permission, so the role gate does not apply to it — a channel
    /// that requires subscribers in chat would otherwise silently refuse the points a non-subscriber
    /// had already paid.
    async fn handle_request(
        self: &Arc<Self>,
        user_login: &str,
        user_name: &str,
        badge_sets: &[&str],
        query: Option<String>,
        from_reward: bool,
    ) -> requests::Outcome {
        let (enabled, min_role, user_cd, global_cd, reply_in_chat) = {
            let inner = self.inner.lock().await;
            let c = &inner.config;
            (
                c.requests_enabled,
                permissions::Role::parse(&c.min_role).unwrap_or(permissions::Role::Everyone),
                c.user_cooldown_secs,
                c.global_cooldown_secs,
                c.reply_in_chat,
            )
        };

        let outcome = if !enabled {
            // Silence would be worse than a refusal here: requests switched off is a deliberate
            // state, and a viewer who gets nothing assumes the bot is broken.
            requests::Outcome::NotAllowed(permissions::Role::Everyone)
        } else if !from_reward && !permissions::allows(min_role, badge_sets) {
            requests::Outcome::NotAllowed(min_role)
        } else if query.is_none() {
            requests::Outcome::NothingAsked
        } else {
            let now = std::time::Instant::now();
            let refusal = {
                let cds = self.cooldowns(user_cd, global_cd).await;
                cds.check(user_login, now).err()
            };

            match refusal {
                Some(refusal) => requests::Outcome::TooSoon(refusal),
                None => {
                    let query = query.expect("checked above");
                    match self.ask_the_app(query, user_login).await {
                        None => requests::Outcome::LookupFailed,
                        Some(outcome) => {
                            // Recorded only on success. Recording the attempt would lock a viewer
                            // out for asking about a song that was not found, which punishes them
                            // for the catalogue's gap.
                            if outcome.is_success() {
                                self.cooldowns(user_cd, global_cd).await.record(user_login, now);
                            }
                            outcome
                        }
                    }
                }
            }
        };

        self.answered.fetch_add(1, Ordering::Relaxed);
        if outcome.is_success() {
            self.queued.fetch_add(1, Ordering::Relaxed);
        }
        if reply_in_chat {
            self.say(&outcome.reply(user_name)).await;
        }
        outcome
    }

    /// Send the request down the channel and wait for the app's answer.
    ///
    /// `None` means there was nobody listening — the bridge was never taken, or the app is shutting
    /// down. Both are states the viewer cannot act on, so the caller turns them into a generic
    /// "try again" rather than reporting a cause.
    async fn ask_the_app(&self, query: String, user_login: &str) -> Option<requests::Outcome> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.commands
            .send(TwitchCommand::Request {
                query,
                from: requests::source_label(user_login),
                reply: tx,
            })
            .ok()?;
        rx.await.ok()
    }

    /// Say something in the channel.
    ///
    /// A failure is logged and swallowed. The song is already in the queue by the time this runs, so
    /// letting a chat problem fail the request would lose something that had already worked.
    async fn say(self: &Arc<Self>, text: &str) {
        let (client_id, broadcaster, sender) = {
            let inner = self.inner.lock().await;
            (inner.client_id.clone(), inner.config.channel_id.clone(), inner.token_user_id.clone())
        };
        let (Some(broadcaster), Some(sender)) = (broadcaster, sender) else {
            return;
        };
        let Ok(token) = self.access_token().await else {
            return;
        };
        if let Err(e) = api::Helix::new(self.http(), &client_id, &token)
            .send_chat_message(&broadcaster, &sender, text)
            .await
        {
            tracing::debug!(error = %e, "twitch: could not answer in chat");
        }
    }

    /// A chat message arrived. Decide whether it asked for anything.
    async fn on_chat_message(self: &Arc<Self>, message: ChatMessage) {
        let (enabled, commands) = {
            let inner = self.inner.lock().await;
            let c = &inner.config;
            let aliases: Vec<&str> = c.request_aliases.iter().map(String::as_str).collect();
            (c.requests_enabled, chat::Commands::new(&c.command_prefix, &aliases))
        };
        if !enabled {
            return;
        }

        // `Bare` is the command with nothing after it, which earns a reply saying how to use it.
        // Anything else is not addressed to us, and answering it would make the bot noise.
        let query = match commands.parse(&message.text) {
            chat::Parsed::Request(raw) => chat::clean_query(raw),
            chat::Parsed::Bare => None,
            chat::Parsed::Other(_) | chat::Parsed::NotACommand => return,
        };

        let badges: Vec<&str> = message.badges.iter().map(|b| b.set_id.as_str()).collect();
        self.handle_request(
            &message.chatter_user_login,
            &message.chatter_user_name,
            &badges,
            query,
            false,
        )
        .await;
    }

    /// A Channel Points redemption arrived.
    async fn on_redemption(self: &Arc<Self>, event: &serde_json::Value) {
        let (enabled, configured) = {
            let inner = self.inner.lock().await;
            (inner.config.requests_enabled, inner.config.reward_id.clone())
        };
        if !enabled {
            return;
        }

        let redemption = match rewards::Redemption::from_event(event) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, "twitch: unparseable redemption");
                return;
            }
        };
        // An unconfigured reward matches nothing, which is the only safe default: the alternative
        // turns every reward in the channel into a song request.
        if !redemption.is_actionable(&configured) {
            return;
        }

        self.handle_request(
            &redemption.user_login,
            &redemption.user_name,
            &[],
            redemption.query(),
            true,
        )
        .await;
    }

    // --- user actions --------------------------------------------------------------------------

    /// Set (or clear) the client ID. Not a secret, so the UI may do this.
    pub async fn set_client_id(self: &Arc<Self>, client_id: &str) -> Result<(), String> {
        let trimmed = client_id.trim().to_owned();
        if !trimmed.is_empty() && !trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("A Twitch client ID is 30 lowercase letters and digits.".into());
        }
        self.db.set_setting(KEY_CLIENT_ID, &trimmed);
        {
            let mut inner = self.inner.lock().await;
            inner.client_id = if trimmed.is_empty() {
                settings::bundled_client_id().trim().to_owned()
            } else {
                trimmed
            };
            inner.error = None;
        }
        self.emit().await;
        Ok(())
    }

    /// Start the device flow. Bumps `gen`, which cancels any flow already in flight.
    pub async fn connect(self: &Arc<Self>) -> Result<(), String> {
        let client_id = { self.inner.lock().await.client_id.clone() };
        if client_id.trim().is_empty() {
            return Err(
                "Set a Twitch client ID first — register an app at dev.twitch.tv/console/apps."
                    .into(),
            );
        }
        let my_gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;

        let device = auth::start_device_flow(self.http(), &client_id).await?;
        let expires_at = crate::db::now_secs() + device.expires_in;
        {
            let mut inner = self.inner.lock().await;
            inner.phase = Phase::Connecting;
            inner.error = None;
            inner.device = Some(DevicePrompt {
                user_code: device.user_code.clone(),
                verification_uri: device.verification_uri.clone(),
                expires_at,
            });
        }
        self.emit().await;

        // The code is also the thing the user has to type, so it goes in the log for anyone who
        // cannot see the window (the app can be hidden in the tray).
        tracing::info!(
            code = %device.user_code,
            url = %device.verification_uri,
            "twitch: waiting for device authorisation"
        );

        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            me.poll_device_flow(my_gen, client_id, device, expires_at).await;
        });
        Ok(())
    }

    /// Poll until the user approves, the code expires, or `gen` moves (cancel/retry/disconnect).
    async fn poll_device_flow(
        self: Arc<Self>,
        my_gen: u64,
        client_id: String,
        device: DeviceCode,
        expires_at: i64,
    ) {
        let mut interval = device.poll_interval();
        loop {
            tokio::time::sleep(interval).await;

            // Cancelled: a newer connect, a cancel, or a disconnect. Leave the state alone —
            // whoever bumped `gen` owns it now.
            if self.gen.load(Ordering::SeqCst) != my_gen {
                return;
            }
            if crate::db::now_secs() >= expires_at {
                self.fail("The Twitch code expired before it was approved. Try again.").await;
                return;
            }

            match auth::poll_device_flow(self.http(), &client_id, &device.device_code).await {
                Poll::Pending => continue,
                Poll::SlowDown => {
                    // Standard device-flow backoff. Not in Twitch's documented error list, but
                    // honouring it costs nothing and ignoring it risks a ban on the endpoint.
                    interval = (interval + Duration::from_secs(1)).min(Duration::from_secs(30));
                    continue;
                }
                Poll::Granted(tokens) => {
                    self.save_tokens(&tokens);
                    match auth::validate(self.http(), &tokens.access_token).await {
                        Ok(v) => self.accept_session(&client_id, *tokens, v).await,
                        Err(e) => {
                            self.fail(format!("Twitch granted a token we could not use: {e}")).await
                        }
                    }
                    return;
                }
                Poll::Failed(message) => {
                    self.fail(format!("Twitch sign-in failed: {message}")).await;
                    return;
                }
            }
        }
    }

    /// Abandon an in-flight device flow, leaving any existing session untouched.
    pub async fn cancel(self: &Arc<Self>) {
        self.gen.fetch_add(1, Ordering::SeqCst);
        {
            let mut inner = self.inner.lock().await;
            inner.device = None;
            if inner.phase == Phase::Connecting {
                inner.phase = Phase::Disconnected;
            }
        }
        self.emit().await;
    }

    /// Sign out. Revokes server-side (best effort) and drops the local tokens either way: a failed
    /// revoke is not a reason to keep someone signed in against their wish.
    pub async fn disconnect(self: &Arc<Self>) {
        self.gen.fetch_add(1, Ordering::SeqCst);
        let client_id = { self.inner.lock().await.client_id.clone() };
        if let Some(tokens) = self.load_tokens() {
            if !client_id.trim().is_empty() {
                if let Err(e) = auth::revoke(self.http(), &client_id, &tokens.access_token).await {
                    tracing::warn!(error = %e, "twitch: revoke failed, dropping local tokens anyway");
                }
            }
        }
        self.clear_tokens();
        // Stop reading chat before the session that authorised it goes away: leaving the socket up
        // would keep delivering messages for an account the user just disconnected.
        self.stop_events().await;
        {
            let mut inner = self.inner.lock().await;
            inner.phase = Phase::Disconnected;
            inner.account = None;
            inner.channel = None;
            inner.device = None;
            inner.scopes.clear();
            inner.expires_at = 0;
            inner.validated_at = 0;
            inner.error = None;
        }
        self.emit().await;
        tracing::info!("twitch: disconnected");
    }

    /// Choose the channel to listen to. Resolving the login to an id needs a token, so this can
    /// only run while connected — and it stores both, because EventSub conditions want the id
    /// while the UI wants the name.
    pub async fn set_channel(self: &Arc<Self>, login: &str) -> Result<(), String> {
        let token = self.access_token().await?;
        let client_id = { self.inner.lock().await.client_id.clone() };

        let login = login.trim();
        if login.is_empty() {
            {
                let mut inner = self.inner.lock().await;
                inner.config.channel_login = None;
                inner.config.channel_id = None;
                inner.channel = None;
            }
            self.persist_config().await;
            // Nothing left to listen to.
            self.stop_events().await;
            return Ok(());
        }

        let user = Helix::new(self.http(), &client_id, &token)
            .user_by_login(login)
            .await
            .map_err(|e| self.explain(e))?;
        let Some(user) = user else {
            return Err(format!("No Twitch channel called \"{login}\"."));
        };

        {
            let mut inner = self.inner.lock().await;
            inner.config.channel_login = Some(user.login.clone());
            inner.config.channel_id = Some(user.id.clone());
            inner.channel = Some(user);
            inner.error = None;
        }
        self.persist_config().await;
        // Restart on the new channel. `start_events_if_ready` bumps the generation, so the loop
        // reading the old channel stops — without this the app would sit subscribed to a channel
        // the user just replaced.
        self.stop_events().await;
        self.start_events_if_ready().await;
        Ok(())
    }

    /// Re-resolve the stored channel against Twitch. Called after a successful validation so the
    /// panel shows the channel's real state rather than a copy from a previous run.
    async fn refresh_channel(self: &Arc<Self>) {
        let login = {
            let inner = self.inner.lock().await;
            if !inner.config.has_channel() {
                return;
            }
            inner.config.channel_login.clone()
        };
        let Some(login) = login else { return };
        if let Err(e) = self.set_channel(&login).await {
            tracing::debug!(error = %e, "twitch: could not refresh the configured channel");
        }
    }

    async fn persist_config(&self) {
        let config = { self.inner.lock().await.config.clone() };
        self.db.set_setting(KEY_CONFIG, &config.to_json());
        self.emit().await;
    }

    /// Turn a [`HelixError`] into something worth showing. The not-an-affiliate case is called out
    /// because it is not a bug and not retryable, and Twitch's own wording does not explain it.
    fn explain(&self, e: HelixError) -> String {
        if e.is_not_affiliate() {
            return "That channel is not a Twitch affiliate or partner, so Channel Points \
                    rewards are unavailable there."
                .into();
        }
        match e {
            HelixError::Unauthorized(_) => "Twitch rejected the session. Connect again.".into(),
            other => other.message().to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The event name is the contract with `api.ts`; pin it.
    #[test]
    fn event_name_is_pinned() {
        assert_eq!(EVENT_STATE, "tw-state");
    }

    /// Twitch asks for validation hourly. A shorter interval is not free — it spends rate-limit
    /// budget — so the constant must not drift.
    #[test]
    fn validation_interval_is_an_hour() {
        assert_eq!(VALIDATE_EVERY, Duration::from_secs(3600));
    }

    /// Phases serialise lowercase, which is what the Svelte side switches on.
    #[test]
    fn phases_serialise_lowercase() {
        assert_eq!(serde_json::to_string(&Phase::Disconnected).unwrap(), r#""disconnected""#);
        assert_eq!(serde_json::to_string(&Phase::Connecting).unwrap(), r#""connecting""#);
        assert_eq!(serde_json::to_string(&Phase::Connected).unwrap(), r#""connected""#);
    }

    /// The snapshot is what reaches the webview, so this is the last line of defence for the
    /// secrets rule. If a token field is ever added to `Snapshot` by mistake, this fails.
    #[test]
    fn snapshot_never_carries_a_token() {
        let snapshot = Snapshot {
            phase: Phase::Connected,
            client_id: "abc".into(),
            client_id_bundled: false,
            configured: true,
            device: Some(DevicePrompt {
                user_code: "ABCD-EFGH".into(),
                verification_uri: "https://www.twitch.tv/activate".into(),
                expires_at: 1,
            }),
            account: None,
            channel: None,
            channel_points_available: false,
            config: TwitchConfig::default(),
            eventsub: EventSubSnapshot::default(),
            scopes: vec!["bits:read".into()],
            expires_at: 2,
            validated_at: 3,
            error: None,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        // The device code is a credential for the flow in progress, but the *user code* is meant
        // to be shown; `device_code` is not part of the prompt and must not appear.
        assert!(json.contains("ABCD-EFGH"), "the user code is shown on purpose");
        for forbidden in ["access_token", "refresh_token", "device_code", "Bearer"] {
            assert!(!json.contains(forbidden), "{forbidden} must not reach the UI");
        }
    }

    /// The snapshot uses camelCase for the Svelte side, matching the rest of `api.ts`.
    #[test]
    fn snapshot_is_camel_case() {
        let snapshot = Snapshot {
            phase: Phase::Disconnected,
            client_id: String::new(),
            client_id_bundled: false,
            configured: false,
            device: None,
            account: None,
            channel: None,
            channel_points_available: false,
            config: TwitchConfig::default(),
            eventsub: EventSubSnapshot::default(),
            scopes: vec![],
            expires_at: 0,
            validated_at: 0,
            error: None,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("\"clientId\""));
        assert!(json.contains("\"validatedAt\""));
        // Nested structs must follow the payload, not serde's default: a snake_case object sitting
        // inside a camelCase payload is exactly the kind of mismatch that costs an hour in the
        // TypeScript.
        assert!(json.contains("\"channelPointsAvailable\""));
        assert!(json.contains("\"autoConnect\""), "TwitchConfig serialises camelCase too");
        assert!(json.contains("\"channelLogin\""));
        assert!(!json.contains("\"client_id\""));
        assert!(!json.contains("\"auto_connect\""));
    }
}
