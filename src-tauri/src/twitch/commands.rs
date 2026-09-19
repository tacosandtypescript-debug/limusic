//! The `tw_*` Tauri commands — the only door between the webview and the Twitch session.
//!
//! They live in the module rather than in `src-tauri/src/commands.rs` on purpose. The app's
//! command file is 1,600 lines and grows with every feature; keeping ours here means the entire
//! Twitch surface is one directory, and a rebase against an upstream LiMusic release has nothing
//! to resolve in that file. The same reasoning as the rest of the module: the integration should
//! be removable by deleting a folder and five lines of wiring.
//!
//! Nothing here returns a token. [`Snapshot`] is hand-built for that reason, and the client ID —
//! which is not a secret — is the only credential-shaped value that crosses.

use std::sync::Arc;

use tauri::State;

use super::{Snapshot, TwitchSession};

type Tw<'a> = State<'a, Arc<TwitchSession>>;

/// Everything the Settings ▸ Twitch panel needs: the session, plus the config it embeds.
///
/// There is deliberately no separate `tw_get_config`. The configured channel has to be visible
/// while **disconnected** (the panel shows "watching X" before you connect, and the Clear button
/// depends on it), so the snapshot has to carry the config anyway — a second command returning the
/// same bytes would be a duplicate read path with its own chance to drift.
#[tauri::command]
pub async fn tw_status(tw: Tw<'_>) -> Result<Snapshot, String> {
    Ok(tw.inner().snapshot().await)
}

/// Store the user's Twitch application client ID. A client ID is public (it travels in a header on
/// every request), so this is the one credential-shaped value the webview may write — which is what
/// lets a user paste their own instead of rebuilding the app.
#[tauri::command]
pub async fn tw_set_client_id(tw: Tw<'_>, client_id: String) -> Result<(), String> {
    tw.inner().set_client_id(&client_id).await
}

/// Begin the Device Code flow. Resolution arrives through the `tw-state` event, not this promise:
/// the user has to leave for twitch.tv and come back, which outlives any reasonable command call.
#[tauri::command]
pub async fn tw_connect(tw: Tw<'_>) -> Result<(), String> {
    tw.inner().clone().connect().await
}

/// Abandon a device flow that is waiting for approval, leaving any existing session alone.
#[tauri::command]
pub async fn tw_cancel(tw: Tw<'_>) -> Result<(), String> {
    tw.inner().clone().cancel().await;
    Ok(())
}

/// Sign out and revoke the token server-side (best effort).
#[tauri::command]
pub async fn tw_disconnect(tw: Tw<'_>) -> Result<(), String> {
    tw.inner().clone().disconnect().await;
    Ok(())
}

/// Choose the channel the bot listens to. Pass an empty string to clear it.
///
/// Resolves the login to a numeric id, which is what every later EventSub condition needs, and
/// stores both so the panel can show a name and the API can be given an id.
#[tauri::command]
pub async fn tw_set_channel(tw: Tw<'_>, login: String) -> Result<(), String> {
    tw.inner().clone().set_channel(&login).await
}
