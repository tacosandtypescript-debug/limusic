//! Phase 1 of the TikTok integration: prove a real message can be read, and find out what a
//! moderator looks like on the wire.
//!
//! Run it against a live stream:
//!
//! ```text
//! cargo run --example tt_probe -- <tiktok-username>
//! ```
//!
//! It is an example rather than a binary on purpose. Examples are not part of the app, are not
//! packaged, and are deleted by removing one file — which is the same removability the whole
//! integration is supposed to have. Nothing here belongs in the shipped product.
//!
//! ## Why it prints so much
//!
//! Phase 2 needs to know which field says "moderator", and the answer is not documented anywhere
//! authoritative — the protocol is scraped and the field has changed shape at least once. So rather
//! than guessing at `user_role`, this prints every field of the sender that could possibly carry the
//! answer for the first messages that arrive, and the value is read off real traffic.
//!
//! The two candidates, from `piratetok_live_rs::structs::proto::user::UserIdentity`:
//!
//! - `user_role` (tag 47): an int, meaning undocumented but plausibly the role.
//! - `badge_list` (tag 64): the modern badge payload — the one the other Rust connector omits.
//!
//! If a moderator's message shows nothing distinguishable in either, the permission ladder is cut
//! back to the streamer and everyone, and that gets said rather than worked around.

use piratetok_live_rs::structs::TikTokLiveEvent;
use piratetok_live_rs::TikTokLive;

/// How many chat messages to report in full before going quiet.
///
/// A busy room produces more than can be read, and the point is to look at a handful rather than to
/// log a stream. Everything after this still counts, so the run stays alive and the connection can
/// be watched for a while.
const FULL_REPORTS: usize = 40;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let username = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: cargo run --example tt_probe -- <tiktok-username>");
        std::process::exit(2);
    });

    println!("connecting to @{username} …");
    let mut stream = TikTokLive::builder(&username).connect().await?;
    println!("connected. waiting for chat.\n");

    let mut seen = 0usize;
    while let Some(event) = stream.next_event().await {
        match event {
            TikTokLiveEvent::Chat(msg) => {
                let Some(user) = msg.user.as_ref() else {
                    continue;
                };
                let full = seen < FULL_REPORTS;
                seen += 1;

                if full {
                    // Everything a role decision could rest on, for one real message.
                    println!("--- message {seen} ---");
                    println!("  unique_id   {:?}", user.unique_id);
                    println!("  nickname    {:?}", user.nickname);
                    println!("  user_role   {}", user.user_role);
                    println!("  badge_list  {} entries", user.badge_list.len());
                    for (i, badge) in user.badge_list.iter().enumerate() {
                        // `display_type` says which of the four oneof arms is set; the crate models
                        // the oneof as four `Option` fields rather than an enum, so all four are
                        // printed and the set one shows as `Some`.
                        println!(
                            "      [{i}] display_type={} image={} text={} str={} combine={}",
                            badge.display_type,
                            badge.image_badge.is_some(),
                            badge.text_badge.is_some(),
                            badge.string_badge.is_some(),
                            badge.combine_badge.is_some(),
                        );
                        // The text of a text or string badge is what a moderator badge would carry
                        // if TikTok does not use `user_role` for it.
                        if let Some(t) = badge.text_badge.as_ref() {
                            println!("           text.default_pattern = {:?}", t.default_pattern);
                        }
                        if let Some(s) = badge.string_badge.as_ref() {
                            println!("           str_value = {:?}", s.str_value);
                        }
                        if let Some(c) = badge.combine_badge.as_ref() {
                            println!("           combine.str_value = {:?}", c.str_value);
                        }
                    }
                    println!("  user_badges {} entries", user.user_badges.len());
                    println!("  new_badges  {} entries", user.new_user_badges.len());
                    println!("  fans_club   {}", user.fans_club.is_some());
                    println!("  fans_info   {}", user.fans_club_info.is_some());
                    println!("  subscribe   {}", user.subscribe_info.is_some());
                    println!("  is_follower {}", user.is_follower);
                    println!("  verified    {}", user.verified);
                    println!("  comment     {:?}", msg.comment);
                    println!();
                } else if seen == FULL_REPORTS + 1 {
                    println!("… {FULL_REPORTS} reported; still connected, no longer printing.\n");
                }
            }
            TikTokLiveEvent::Disconnected => {
                println!("disconnected.");
                break;
            }
            _ => {}
        }
    }

    println!("\n{seen} chat messages seen.");
    Ok(())
}
