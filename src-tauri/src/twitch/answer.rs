//! Answering the channel: what phase 3 does when something arrives.
//!
//! An `impl TwitchSession` block that lives away from the struct. Rust allows this — privacy is
//! visible to the defining module and its descendants — and it is what keeps `mod.rs` about the
//! connection rather than about the feature.
//!
//! ## The order of the checks is the design
//!
//! ```text
//! permission -> cooldown -> lookup -> queue
//! ```
//!
//! Each step can only refuse, none can un-refuse, so the first refusal is the answer. Permission
//! comes before the cooldown on purpose: telling somebody to wait implies that waiting will help,
//! and then the wait passes and they are refused again for a reason they were never told.
//!
//! ## What is deliberately not here
//!
//! The search and the enqueue. The session owns the Twitch connection and nothing else, so a
//! request goes down an `mpsc` channel that `lib.rs` applies to `AppState` — the same shape Listen
//! Together uses, and the reason the playback path keeps exactly one owner. The `oneshot` alongside
//! it is what lets the reply say what actually happened instead of what was hoped for.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::*;

impl super::TwitchSession {
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
    pub(super) async fn on_chat_message(self: &Arc<Self>, message: ChatMessage) {
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
    pub(super) async fn on_redemption(self: &Arc<Self>, event: &serde_json::Value) {
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
}
