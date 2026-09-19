//! Loopback HTTP server for the OBS overlay.
//!
//! OBS's Browser Source loads a URL, so the overlay has to *be* a URL. This serves two things on a
//! fixed local port: the page itself (a single self-contained HTML file, `overlay/page.html`) and a
//! `/state` JSON endpoint it polls for what is playing.
//!
//! **Why a socket and not a file.** The page could be written to disk and opened with `file://`,
//! and then it would have no way to ask LiMusic anything — a `file://` page cannot fetch across to
//! a loopback port without CORS, and the whole point is that the overlay follows the music. The
//! loopback socket is the same answer `videoproxy.rs` arrived at for the music video, for the same
//! reason.
//!
//! **Why a fixed port.** Unlike the video proxy, which picks an ephemeral port because nothing
//! outside the app ever needs to know it, this URL is pasted into OBS and has to survive a
//! restart — a port that changed every launch would silently break every configured scene. It
//! defaults to [`DEFAULT_PORT`] and can be moved from Settings.
//!
//! **Why a token in the path.** The same reason as the video proxy: bound to `127.0.0.1`, nothing
//! off the machine can reach it, but any process *on* the machine could otherwise poll it. Unlike
//! the video proxy's, this token is **persisted**, because a token that rotated every launch would
//! break the OBS link it exists to protect.
//!
//! **No CORS headers, deliberately.** A permissionless `Access-Control-Allow-Origin` would let any
//! web page you happen to visit read your listening history from `127.0.0.1` — a real if minor
//! leak. Nothing needs it: the overlay page is same-origin with its own `/state`, and the Settings
//! preview embeds the page in an iframe, which CORS does not govern. A streamer writing their own
//! overlay can read the same JSON from their own page only if they serve it themselves.

use std::convert::Infallible;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Serialize;

use crate::state::AppState;

/// Settings keys. The port is meant to be editable; the token is shown to the user so they can
/// paste the link, but never written back from the UI.
pub const KEY_PORT: &str = "overlay_port";
pub const KEY_TOKEN: &str = "overlay_token";

mod routing;

// The pure half — routing, the token gate, the artwork allowlist — lives in its own file so its
// tests can run without a Tauri app or a socket. See `overlay/routing.rs`.
use routing::{
    asset, cover_allowed, normalize_cover, route, Action, Route, DEFAULT_PORT, MAX_CONTROL_BYTES,
    PAGE,
};

/// What every response is built from.
type ResBody = Full<Bytes>;

static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Endpoint {
    pub port: u16,
    /// Persisted, so the OBS link keeps working across restarts.
    pub token: String,
    /// True when the configured port was busy and we fell back to an ephemeral one.
    pub port_fell_back: bool,
}

/// Bind the listener and start serving. Binds synchronously so [`base_url`] answers the moment this
/// returns, then hands the socket to tokio — the same shape as `videoproxy::start`.
pub fn start(state: Arc<AppState>) {
    let token = match load_or_create_token(&state) {
        Some(t) => t,
        None => {
            tracing::warn!("overlay: could not persist a token; overlay disabled");
            return;
        }
    };
    let wanted = state
        .db
        .get_setting(KEY_PORT)
        .and_then(|p| p.trim().parse::<u16>().ok())
        .filter(|p| *p > 1023)
        .unwrap_or(DEFAULT_PORT);

    let (listener, port, port_fell_back) = match bind(wanted) {
        Some(v) => v,
        None => {
            tracing::warn!(port = wanted, "overlay: could not bind; overlay disabled");
            return;
        }
    };

    if ENDPOINT.set(Endpoint { port, token, port_fell_back }).is_err() {
        return; // already started
    }
    if port_fell_back {
        tracing::warn!(wanted, port, "overlay: port busy, using an ephemeral one");
    }
    tracing::info!(port, "overlay listening on loopback");

    tauri::async_runtime::spawn(async move {
        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    // EMFILE/ENFILE/ENOBUFS return immediately, so `continue` alone would be a
                    // tight loop pinning a core. `videoproxy.rs` learned this the same way.
                    tracing::warn!(error = %e, "overlay: accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let state = state.clone();
            tauri::async_runtime::spawn(async move {
                let svc = service_fn(move |req| serve(req, state.clone()));
                let _ = http1::Builder::new()
                    // hyper 1.x panics the moment it arms a timeout with no timer installed.
                    .timer(TokioTimer::new())
                    .header_read_timeout(Duration::from_secs(15))
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
}

/// Try the requested port, then an ephemeral one. Returns the listener, the port it got, and
/// whether it had to fall back.
fn bind(wanted: u16) -> Option<(TcpListener, u16, bool)> {
    for (port, fell_back) in [(wanted, false), (0, true)] {
        let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, port)) else { continue };
        let Ok(addr) = listener.local_addr() else { continue };
        // Non-blocking before it reaches tokio, or the runtime would own a blocking socket.
        if listener.set_nonblocking(true).is_err() {
            continue;
        }
        return Some((listener, addr.port(), fell_back));
    }
    None
}

/// The persisted token, created on first run.
///
/// Stored in `settings` rather than in memory because the OBS link has to survive a restart. A
/// missing row is not an error — it is the first launch.
fn load_or_create_token(state: &AppState) -> Option<String> {
    if let Some(existing) = state.db.get_setting(KEY_TOKEN).filter(|t| t.len() >= 16) {
        return Some(existing);
    }
    let token = format!("{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>());
    state.db.set_setting(KEY_TOKEN, &token);
    Some(token)
}

/// The URL to paste into OBS, without query parameters.
pub fn base_url() -> Option<String> {
    let ep = ENDPOINT.get()?;
    Some(format!("http://127.0.0.1:{}/{}/", ep.port, ep.token))
}

/// Everything the Settings panel needs to show the link and explain it.
pub fn info() -> Endpoint {
    ENDPOINT.get().cloned().unwrap_or(Endpoint {
        port: 0,
        token: String::new(),
        port_fell_back: false,
    })
}

/// What the Settings ▸ Overlay panel draws itself from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayInfo {
    /// False only when the listener could not be bound at all, which the panel says out loud
    /// rather than showing a link that will not answer.
    pub available: bool,
    pub port: u16,
    /// True when the configured port was busy and the overlay moved to an ephemeral one. The link
    /// still works; it just will not be the port the streamer configured.
    pub port_fell_back: bool,
    /// The URL to paste into OBS, without query parameters. **Contains the token**, on purpose:
    /// this is the value a human copies, so hiding it would make the feature unusable.
    pub base_url: String,
}

/// The link for OBS, plus whether the server is actually up.
#[tauri::command]
pub fn overlay_info() -> OverlayInfo {
    let ep = info();
    OverlayInfo {
        available: ep.port != 0 && !ep.token.is_empty(),
        port: ep.port,
        port_fell_back: ep.port_fell_back,
        base_url: base_url().unwrap_or_default(),
    }
}

fn body(bytes: Bytes) -> ResBody {
    Full::new(bytes)
}

fn respond(status: StatusCode, ctype: &'static str, bytes: &'static [u8]) -> Response<ResBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CONTENT_LENGTH, bytes.len())
        // Never cached: an updated LiMusic must change the overlay on the next OBS refresh, and a
        // stale overlay is indistinguishable from a broken one while you are live.
        .header(header::CACHE_CONTROL, "no-store")
        .body(body(Bytes::from_static(bytes)))
        .expect("static response")
}

fn json(status: StatusCode, value: &serde_json::Value) -> Response<ResBody> {
    let text = value.to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(body(Bytes::from(text)))
        .expect("json response")
}

async fn serve(
    req: Request<hyper::body::Incoming>,
    state: Arc<AppState>,
) -> Result<Response<ResBody>, Infallible> {
    Ok(handle(req, state).await.unwrap_or_else(|status| {
        Response::builder()
            .status(status)
            .header(header::CACHE_CONTROL, "no-store")
            .body(body(Bytes::new()))
            .expect("empty response")
    }))
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    state: Arc<AppState>,
) -> Result<Response<ResBody>, StatusCode> {
    let ep = ENDPOINT.get().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let route = route(req.uri().path(), &ep.token).ok_or(StatusCode::NOT_FOUND)?;
    let method = req.method().clone();
    let readable = matches!(method, Method::GET | Method::HEAD);

    match route {
        Route::Redirect if readable => Response::builder()
            .status(StatusCode::PERMANENT_REDIRECT)
            .header(header::LOCATION, format!("/{}/", ep.token))
            .header(header::CACHE_CONTROL, "no-store")
            .body(body(Bytes::new()))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR),

        Route::Page if readable => {
            // HEAD gets the headers without the page body, which is what a browser or a health
            // check is actually asking for.
            let bytes: &'static [u8] = if method == Method::HEAD { b"" } else { PAGE.as_bytes() };
            Ok(respond(StatusCode::OK, "text/html; charset=utf-8", bytes))
        }

        Route::Asset(name) if readable => {
            let (_, text, ctype) = asset(name).ok_or(StatusCode::NOT_FOUND)?;
            // HEAD gets the headers without the body, as the page does.
            let bytes: &'static [u8] = if method == Method::HEAD { b"" } else { text.as_bytes() };
            Ok(respond(StatusCode::OK, ctype, bytes))
        }

        Route::State if readable => {
            let snapshot = state.playback_snapshot().await;
            Ok(json(StatusCode::OK, &snapshot))
        }

        Route::Cover if readable => {
            // The query is the artwork URL, so it is validated, not trusted.
            let raw = req.uri().query().unwrap_or_default();
            let raw = urlencoding::decode(raw).map_err(|_| StatusCode::BAD_REQUEST)?;
            let normalized = normalize_cover(&raw).ok_or(StatusCode::BAD_REQUEST)?;
            let url = cover_allowed(&normalized).ok_or(StatusCode::FORBIDDEN)?;

            let upstream = crate::http::client()
                .get(url)
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| StatusCode::BAD_GATEWAY)?;
            if !upstream.status().is_success() {
                return Err(StatusCode::BAD_GATEWAY);
            }
            let ctype = upstream
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("image/jpeg")
                .to_owned();
            let bytes = upstream.bytes().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ctype)
                // Artwork for a given URL never changes, and the page requests it per track, so this
                // is the one response worth caching: it keeps the cover from re-crossing the network
                // on every OBS scene switch.
                .header(header::CACHE_CONTROL, "public, max-age=86400")
                .body(body(bytes))
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
        }

        Route::Control if method == Method::POST => {
            let declared = req
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if declared > MAX_CONTROL_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            let raw =
                req.into_body().collect().await.map_err(|_| StatusCode::BAD_REQUEST)?.to_bytes();
            if raw.len() > MAX_CONTROL_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            let parsed: ControlRequest =
                serde_json::from_slice(&raw).map_err(|_| StatusCode::BAD_REQUEST)?;
            let action = Action::parse(&parsed.action).ok_or(StatusCode::BAD_REQUEST)?;
            match action {
                Action::Prev => state.prev_in_queue().await,
                Action::Next => state.next_in_queue().await,
                Action::Toggle => state.resume_or_toggle().await,
            }
            Ok(json(StatusCode::OK, &serde_json::json!({ "ok": true })))
        }

        // Every other combination: a known path with the wrong verb.
        _ => Err(StatusCode::METHOD_NOT_ALLOWED),
    }
}

#[derive(serde::Deserialize)]
struct ControlRequest {
    action: String,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_are_not_cacheable() {
        let html = respond(StatusCode::OK, "text/html; charset=utf-8", PAGE.as_bytes());
        assert_eq!(html.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        // And it must be embeddable, or the Settings preview iframe renders an error page instead
        // of the overlay.
        assert!(html.headers().get("x-frame-options").is_none());
        let state = json(StatusCode::OK, &serde_json::json!({ "paused": false }));
        assert_eq!(state.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
    }

    #[test]
    fn a_busy_port_falls_back_instead_of_failing() {
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let busy = held.local_addr().unwrap().port();
        let (listener, port, fell_back) = bind(busy).expect("must still bind somewhere");
        assert!(fell_back, "should have reported the fallback");
        assert_ne!(port, busy);
        assert!(port > 0);
        drop(listener);
    }
}
