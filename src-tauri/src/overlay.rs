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

/// Where the overlay listens unless the user moves it. High and unremarkable, away from the ports
/// dev servers reach for (3000, 5173, 1420…).
pub const DEFAULT_PORT: u16 = 8799;

/// The page, embedded so the binary is self-contained — the overlay has to work from an installed
/// build with no repo next to it.
const PAGE: &str = include_str!("overlay/page.html");

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

#[derive(Debug, PartialEq)]
enum Route {
    Page,
    State,
    /// `POST` with `{"action": "…"}`. What makes the overlay's transport buttons real.
    Control,
    /// Artwork, fetched by us and handed to the overlay. See [`cover_allowed`].
    Cover,
    /// `/token` with no trailing slash. Redirected rather than served, so the page's own relative
    /// URLs and the browser's idea of the base stay in agreement.
    Redirect,
}

/// Turn what YouTube hands us into something fetchable, or `None` if it is unusable.
///
/// InnerTube returns thumbnails as absolute `https://` URLs today, but it also returns
/// **protocol-relative** ones (`//lh3.googleusercontent.com/…`) and has for years. Handed to an
/// `Image` from a `file://` page, `//host/path` resolves to `file://host/path` and fails with
/// nothing a page can report — which is exactly what "the cover does not load" looks like.
fn normalize_cover(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.strip_prefix("//") {
        Some(rest) => Some(format!("https://{rest}")),
        None => Some(trimmed.to_owned()),
    }
}

/// Which hosts artwork may be fetched from.
///
/// `/cover` takes a URL, so without this it would be an open proxy: anything able to reach the port
/// could make LiMusic fetch an arbitrary address, including one inside the local network. This is
/// the set YouTube Music actually serves thumbnails from.
///
/// Pure, so the rule is testable without a socket.
fn cover_allowed(url: &str) -> Option<reqwest::Url> {
    let parsed = reqwest::Url::parse(url).ok()?;
    // https only: plain http would be a downgrade we do not need.
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?;
    let ok = ["googleusercontent.com", "ggpht.com", "ytimg.com"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    ok.then_some(parsed)
}

/// The three things the overlay can ask for. A closed set, parsed rather than passed through: this
/// endpoint is reachable by any local process that knows the token, so it must not be a general
/// "call a method by name" hole.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Prev,
    Next,
    Toggle,
}

impl Action {
    fn parse(raw: &str) -> Option<Action> {
        match raw {
            "prev" => Some(Action::Prev),
            "next" => Some(Action::Next),
            "toggle" => Some(Action::Toggle),
            _ => None,
        }
    }
}

/// The largest control body worth reading. The real one is ~25 bytes; anything larger is either a
/// mistake or someone probing, and buffering it unbounded is the one way this endpoint could be
/// made to cost memory.
const MAX_CONTROL_BYTES: usize = 1024;

/// Split a request path into a route, refusing anything whose token does not match.
///
/// Pure, so the token check — the only thing standing between a local process and the endpoint —
/// is testable without opening a socket.
fn route(path: &str, token: &str) -> Option<Route> {
    if token.is_empty() {
        return None;
    }
    let rest = path.strip_prefix('/')?;
    match rest.split_once('/') {
        // "/<token>"
        None => (rest == token).then_some(Route::Redirect),
        Some((t, tail)) => {
            if t != token {
                return None;
            }
            match tail {
                "" | "index.html" => Some(Route::Page),
                "state" => Some(Route::State),
                "control" => Some(Route::Control),
                "cover" => Some(Route::Cover),
                _ => None,
            }
        }
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

    const T: &str = "abc123def456abc1";

    /// The token is the only thing keeping another local process off the endpoint, so a wrong or
    /// missing one has to miss — for every shape of path.
    #[test]
    fn only_the_right_token_routes() {
        assert_eq!(route(&format!("/{T}/"), T), Some(Route::Page));
        assert_eq!(route(&format!("/{T}/state"), T), Some(Route::State));
        assert_eq!(route("/wrongtoken000000/", T), None);
        assert_eq!(route("/", T), None);
        assert_eq!(route("/state", T), None);
        assert_eq!(route(T, T), None);
        // A token that is a prefix of the path must not match by prefix.
        assert_eq!(route(&format!("/{T}extra/"), T), None);
        assert_eq!(route(&format!("/{T}extra/state"), T), None);
    }

    /// An empty token would make every path a match, so the guard has to reject it outright rather
    /// than fall through to a comparison that succeeds for `"/"`.
    #[test]
    fn an_empty_token_routes_nothing() {
        assert_eq!(route("/", ""), None);
        assert_eq!(route("//", ""), None);
        assert_eq!(route("//state", ""), None);
    }

    /// `/token` without the slash is a redirect, so a hand-typed link works and the page keeps a
    /// consistent base for its own relative URLs.
    #[test]
    fn a_bare_token_redirects() {
        assert_eq!(route(&format!("/{T}"), T), Some(Route::Redirect));
    }

    #[test]
    fn unknown_paths_under_a_valid_token_are_not_found() {
        assert_eq!(route(&format!("/{T}/../secret"), T), None);
        assert_eq!(route(&format!("/{T}/favicon.ico"), T), None);
        assert_eq!(route(&format!("/{T}/state/extra"), T), None);
        assert_eq!(route(&format!("/{T}/control/../state"), T), None);
    }

    /// `/cover` fetches a URL from a query parameter, so the host allowlist is the only thing
    /// between it and an open proxy — including one that could reach addresses inside the local
    /// network. Every rejection path matters more than the accept path.
    #[test]
    fn cover_only_fetches_youtube_artwork_hosts() {
        for good in [
            "https://lh3.googleusercontent.com/abc=w544-h544",
            "https://yt3.ggpht.com/xyz",
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
            "https://googleusercontent.com/a",
        ] {
            assert!(cover_allowed(good).is_some(), "{good} should be allowed");
        }
        for bad in [
            // Not the allowlisted domains.
            "https://example.com/a.jpg",
            "https://evil-googleusercontent.com.attacker.net/a",
            "https://notgoogleusercontent.com/a",
            // Plain http: a downgrade we do not need.
            "http://lh3.googleusercontent.com/a",
            // The classic SSRF targets.
            "http://127.0.0.1:8799/secret",
            "https://localhost/admin",
            "https://169.254.169.254/latest/meta-data/",
            "file:///C:/Windows/win.ini",
            "data:image/svg+xml,<svg/>",
            // Nonsense.
            "",
            "not a url",
        ] {
            assert!(cover_allowed(bad).is_none(), "{bad} must be refused");
        }
    }

    /// Protocol-relative thumbnails are the whole reason this exists: off a `file://` page they
    /// resolve to `file://host/…` and fail with nothing a page can report.
    #[test]
    fn protocol_relative_covers_become_https() {
        assert_eq!(
            normalize_cover("//lh3.googleusercontent.com/a=b").as_deref(),
            Some("https://lh3.googleusercontent.com/a=b")
        );
        // Absolute URLs pass through untouched, including their query strings.
        assert_eq!(
            normalize_cover("https://i.ytimg.com/vi/x/hq.jpg?sqp=1").as_deref(),
            Some("https://i.ytimg.com/vi/x/hq.jpg?sqp=1")
        );
        // Whitespace from a pasted or hand-edited value.
        assert_eq!(
            normalize_cover("  https://lh3.googleusercontent.com/a  ").as_deref(),
            Some("https://lh3.googleusercontent.com/a")
        );
        assert_eq!(normalize_cover(""), None);
        assert_eq!(normalize_cover("   "), None);
    }

    /// A `//host` that is not on the allowlist must still be refused after normalisation — the two
    /// checks compose, and either one alone would leave a hole.
    #[test]
    fn normalisation_does_not_bypass_the_allowlist() {
        let normalized = normalize_cover("//169.254.169.254/latest/meta-data/").unwrap();
        assert_eq!(normalized, "https://169.254.169.254/latest/meta-data/");
        assert!(cover_allowed(&normalized).is_none());
    }

    #[test]
    fn the_control_endpoint_routes() {
        assert_eq!(route(&format!("/{T}/control"), T), Some(Route::Control));
        // And it is behind the same token as everything else.
        assert_eq!(route("/wrongtoken000000/control", T), None);
    }

    /// The control endpoint is reachable by any local process holding the token, so the action set
    /// is closed and parsed — never a name passed through to a dispatcher.
    #[test]
    fn only_the_three_known_actions_parse() {
        assert_eq!(Action::parse("prev"), Some(Action::Prev));
        assert_eq!(Action::parse("next"), Some(Action::Next));
        assert_eq!(Action::parse("toggle"), Some(Action::Toggle));
        for bad in ["", "NEXT", "Next", "play", "shutdown", "__proto__", "next; rm -rf /"] {
            assert_eq!(Action::parse(bad), None, "{bad:?} must not parse");
        }
    }

    /// A body larger than the threshold is refused instead of buffered.
    #[test]
    fn the_control_body_is_bounded() {
        assert!(MAX_CONTROL_BYTES >= 64, "too small to hold a real request");
        assert!(MAX_CONTROL_BYTES <= 8 * 1024, "too large to be a real boundary");
        // A real request is ~25 bytes, comfortably inside.
        let real = br#"{"action":"toggle"}"#;
        assert!(real.len() < MAX_CONTROL_BYTES);
    }

    /// The page must never be cached: a streamer refreshing the browser source has to get the
    /// build that is actually running.
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

    /// The page is shipped from the binary, so a missing or truncated file is a build-time problem
    /// that would otherwise only show up as a blank overlay on stream. Pin the parts it needs.
    #[test]
    fn the_embedded_page_is_intact() {
        assert!(PAGE.len() > 8_000, "page looks truncated: {} bytes", PAGE.len());
        // All three designs must be reachable by the query parameter the Settings panel sends.
        for design in ["sleeve", "playout", "vinyl"] {
            assert!(PAGE.contains(design), "page has no {design} design");
        }
        // The four knobs the link exposes, and the two properties that make it an overlay at all.
        for needle in ["design", "pos", "scale", "demo"] {
            assert!(PAGE.contains(needle), "page does not read {needle}");
        }
        assert!(PAGE.contains("background: transparent"), "the overlay must not paint a backdrop");
        assert!(PAGE.contains("prefers-reduced-motion"), "motion must be suppressed on request");
        // Polling has to be built from the page's own path, because the endpoint lives under the
        // token (`/<token>/state`) — a bare `/state` fetch silently 404s and the overlay stays blank.
        assert!(
            PAGE.contains(r#"location.pathname.replace"#),
            "the state URL must be derived from the page's own path"
        );
        assert!(!PAGE.contains(r#"fetch("/state""#), "a bare /state fetch would miss the token");
    }

    /// A port below 1024 needs privileges on some systems and collides with well-known services, so
    /// the stored value is filtered rather than trusted.
    #[test]
    fn unusable_stored_ports_fall_back_to_the_default() {
        let pick = |raw: Option<&str>| {
            raw.and_then(|p| p.trim().parse::<u16>().ok())
                .filter(|p| *p > 1023)
                .unwrap_or(DEFAULT_PORT)
        };
        assert_eq!(pick(None), DEFAULT_PORT);
        assert_eq!(pick(Some("")), DEFAULT_PORT);
        assert_eq!(pick(Some("not a port")), DEFAULT_PORT);
        assert_eq!(pick(Some("80")), DEFAULT_PORT, "privileged");
        assert_eq!(pick(Some("1023")), DEFAULT_PORT, "still privileged");
        assert_eq!(pick(Some("9000")), 9000);
        assert_eq!(pick(Some("  9123  ")), 9123);
    }

    /// `bind` on a port that is already taken must fall through to an ephemeral one instead of
    /// leaving the streamer with no overlay and no explanation.
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
