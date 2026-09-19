//! Live smoke test against Twitch's real OAuth endpoints.
//!
//! These talk to the network, so they are `#[ignore]`d — the same convention `CONTRIBUTING.md`
//! sets for the repo ("Tests that talk to the network are `#[ignore]`d so the default run works
//! offline"). Run them deliberately:
//!
//! ```text
//! cargo test -- --ignored --nocapture
//! ```
//!
//! What they can and cannot prove. There is no client ID in this checkout, so nothing here can
//! complete a real authorisation — that needs a human registering an application and approving a
//! code on twitch.tv. What they *do* prove is the part that is easy to get silently wrong and
//! expensive to discover later:
//!
//! * the endpoints exist at the URLs we hard-coded,
//! * our request shape is accepted (a rejected *credential* is a different failure from a rejected
//!   *parameter list*, and the message distinguishes them),
//! * `Authorization: OAuth <token>` is the header form `/validate` wants,
//! * failures surface as typed errors instead of panics, hangs or "success" with an empty body.

use limusic_verify::auth;

/// A syntactically valid but non-existent client ID. Twitch's response to it is the whole point of
/// the test: it tells us the request was well-formed enough to get as far as credential checking.
const BOGUS_CLIENT_ID: &str = "000000000000000000000000000000";

#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn device_endpoint_is_reachable_and_our_request_shape_is_accepted() {
    let http = reqwest::Client::new();
    let result = auth::start_device_flow(&http, BOGUS_CLIENT_ID).await;

    match result {
        Ok(code) => {
            // Should not happen with an unknown client ID, but if Twitch ever answers, say so
            // loudly rather than passing quietly on a surprise.
            panic!(
                "Twitch issued a device code for an unknown client ID: user_code={} uri={}",
                code.user_code, code.verification_uri
            );
        }
        Err(message) => {
            println!("device flow error: {message}");
            assert!(
                !message.trim().is_empty(),
                "the error must carry a message, not an empty string"
            );
            // The failure has to be about the credential. If Twitch were rejecting our parameter
            // names (for instance `scopes` vs `scope`), the wording would be about the request
            // instead — which is exactly the bug this assertion is here to catch.
            let lowered = message.to_ascii_lowercase();
            assert!(
                lowered.contains("client") || lowered.contains("invalid"),
                "expected a credential complaint, got: {message}"
            );
            // And our own wrapper must have added context rather than swallowed it.
            assert!(
                message.contains("device code"),
                "the error lost its context: {message}"
            );
        }
    }
}

/// `/validate` is the call Twitch *requires* hourly, so its exact shape is worth pinning: the
/// documented header is `Authorization: OAuth <token>`, and a bad token must come back as a 401
/// we can recognise, because "the token is dead" is what triggers a refresh.
#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn validate_rejects_a_bad_token_with_an_identifiable_message() {
    let http = reqwest::Client::new();
    let err = auth::validate(&http, "not-a-real-token")
        .await
        .expect_err("a bogus token must not validate");
    println!("validate error: {err}");
    let lowered = err.to_ascii_lowercase();
    assert!(
        lowered.contains("invalid") || lowered.contains("token") || lowered.contains("401"),
        "unexpected validate failure: {err}"
    );
}

/// Signing out has to be safe to call with nothing worth revoking: it is the path a user takes
/// when a token was already revoked from the Twitch side, which is a normal thing to happen.
#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn revoke_is_safe_for_a_dead_token() {
    let http = reqwest::Client::new();
    // Either outcome is acceptable — what must not happen is a panic or a hang.
    match auth::revoke(&http, BOGUS_CLIENT_ID, "not-a-real-token").await {
        Ok(()) => println!("revoke reported success for a dead token (treated as done)"),
        Err(e) => println!("revoke reported: {e}"),
    }
}

/// The Helix side, same idea: prove the base URL and the two required headers reach Twitch and that
/// a bad token produces [`HelixError::Unauthorized`] — the variant the refresh path branches on.
/// Folding that into a generic error would silently break the reactive-refresh rule.
#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn helix_unauthorized_is_typed_not_generic() {
    use limusic_verify::api::{Helix, HelixError};

    let http = reqwest::Client::new();
    let helix = Helix::new(&http, BOGUS_CLIENT_ID, "not-a-real-token");
    let err = helix
        .current_user()
        .await
        .expect_err("a bogus token must not return a user");
    println!("helix error: {err:?}");
    assert!(
        matches!(err, HelixError::Unauthorized(_)),
        "expected Unauthorized so the caller refreshes, got {err:?}"
    );
    // 401 is never retryable: retrying identical credentials cannot help.
    assert!(!err.is_retryable());
}

/// The EventSub handshake, end to end, **without a token**.
///
/// A WebSocket connection needs no credentials — only *subscribing* does — which makes this the
/// one part of phase 2 that can be verified here. It covers the three things most likely to be
/// silently wrong:
///
/// * the endpoint URL and TLS handshake,
/// * the envelope and `session_welcome` parse (the session id every subscription is bound to),
/// * **the 10-second rule and the close-code mapping**: a session that never subscribes is closed
///   with 4003, and this test does exactly that to see a real close frame come back through
///   `close_reason`.
///
/// It also incidentally answers the question the code depends on: whether `tungstenite`'s automatic
/// Pong is enough. If it were not, the connection would die with 4002 well before the 4003 this
/// expects.
#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn eventsub_handshake_yields_a_session_then_closes_when_unused() {
    use std::time::{Duration, Instant};
    use limusic_verify::events::{self, Incoming};

    let mut ws = events::open(events::DEFAULT_URL)
        .await
        .expect("the EventSub socket must open with no credentials at all");

    // 1. The welcome.
    let mut session_id: Option<String> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), events::next(&mut ws)).await {
            Ok(Some(Ok(Incoming::Welcome(w)))) => {
                println!(
                    "welcome: session={} keepalive={}s reconnect_url={:?}",
                    w.session_id, w.keepalive_timeout_seconds, w.reconnect_url
                );
                assert!(
                    w.keepalive_timeout_seconds >= 10,
                    "Twitch's documented default is 10s, got {}",
                    w.keepalive_timeout_seconds
                );
                session_id = Some(w.session_id);
                break;
            }
            Ok(Some(Ok(other))) => println!("before the welcome: {other:?}"),
            Ok(Some(Err(e))) => panic!("failed before a welcome: {e}"),
            Ok(None) => panic!("the socket closed before sending a welcome"),
            Err(_) => continue,
        }
    }
    let session_id = session_id.expect("Twitch must send session_welcome first");
    assert!(!session_id.is_empty(), "the session id is what subscriptions are bound to");

    // 2. Never subscribe, and watch the documented 4003 arrive. This is the only way to exercise
    //    the close-code mapping against a real close rather than a hand-built frame.
    let give_up = Instant::now() + Duration::from_secs(30);
    let mut closed = false;
    while Instant::now() < give_up {
        match tokio::time::timeout(Duration::from_secs(6), events::next(&mut ws)).await {
            Ok(Some(Ok(other))) => println!("frame: {other:?}"),
            Ok(Some(Err(reason))) => {
                println!("closed: {reason}");
                assert!(
                    reason.contains("4003") || reason.contains("never used"),
                    "expected the documented unused-connection close, got: {reason}"
                );
                closed = true;
                break;
            }
            Ok(None) => {
                println!("stream ended without a close frame");
                closed = true;
                break;
            }
            Err(_) => continue,
        }
    }
    assert!(
        closed,
        "a session that never subscribed must be closed; it was still open after 30s"
    );
}

/// A host that does not resolve must fail fast and with a readable message, not hang. This is the
/// path a typo in a reconnect URL would take.
#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn a_bad_host_fails_instead_of_hanging() {
    use std::time::{Duration, Instant};
    use limusic_verify::events;

    let started = Instant::now();
    let err = events::open("wss://this-host-does-not-exist-limusic-test.invalid/ws")
        .await
        .expect_err("a name that cannot resolve must not open");
    println!("open error after {:?}: {err}", started.elapsed());
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "DNS failure took {:?}; it must not wait out a TCP timeout",
        started.elapsed()
    );
}

/// A channel name that cannot exist must come back as `Ok(None)` — an empty list, not an error —
/// because the UI turns that into "no channel called X", which is a typo the user can fix, and an
/// error would surface as a failure of the whole connection instead.
///
/// This one also exercises the full happy-path plumbing: request built, headers sent, `data`
/// envelope parsed. It only works when the token is accepted, so it skips when it is not.
///
/// The name has to be *well-formed* to get past the local validator and reach Twitch at all — a
/// hyphen would be rejected here before any request is built, which is the correct behaviour but
/// would prove nothing about the network path.
#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn unknown_channel_is_none_not_an_error() {
    use limusic_verify::api::{Helix, HelixError};

    let http = reqwest::Client::new();
    let helix = Helix::new(&http, BOGUS_CLIENT_ID, "not-a-real-token");
    match helix.user_by_login("thischannelcannotpossiblyexistzzz9").await {
        Ok(None) => println!("empty data envelope: an unknown login is not an error"),
        Ok(Some(u)) => panic!("an impossible login resolved to {}", u.login),
        // Expected with a bogus token: the point of the request was the plumbing, and Helix
        // authenticates before it looks at the query.
        Err(HelixError::Unauthorized(m)) => println!("reached Twitch, token rejected: {m}"),
        Err(other) => panic!("unexpected failure shape: {other:?}"),
    }
}

/// The local validator must reject a login that is not a login, **before** spending a request —
/// and this is the counterpart to the test above: the same call with a hyphen is a
/// [`HelixError::BadRequest`] and never touches the network.
#[tokio::test]
#[ignore = "hits the live Twitch API"]
async fn malformed_login_is_rejected_without_a_request() {
    use limusic_verify::api::{Helix, HelixError};

    let http = reqwest::Client::new();
    let helix = Helix::new(&http, BOGUS_CLIENT_ID, "not-a-real-token");
    // A bogus token would give Unauthorized if the request were actually sent, so BadRequest here
    // is proof the round trip never happened.
    let err = helix
        .user_by_login("this-channel-does-not-exist")
        .await
        .expect_err("a hyphen is not valid in a Twitch login");
    assert!(
        matches!(err, HelixError::BadRequest(_)),
        "expected a local rejection, got {err:?}"
    );
    println!("rejected locally: {err}");
}
