//! The pure half of the overlay server: what a request is, and what it may fetch.
//!
//! Split out so its tests can actually run. They could not before: the module they lived in pulls in
//! `AppState` and hyper, so its test binary needs a Tauri app and — on this machine — a `libmpv`
//! runtime that the app's own test binary cannot load (`STATUS_ENTRYPOINT_NOT_FOUND`, 0xc0000139).
//! The token gate and the artwork allowlist are the two things standing between a local process and
//! an open proxy, and both were compiled but never executed. Here they are executed.
//!
//! Nothing in this file touches Tauri, hyper, or the filesystem.

/// The port the overlay is served on, unless settings say otherwise.
pub const DEFAULT_PORT: u16 = 8799;

/// The shell: the markup, and links to everything below.
///
/// It was one file until it reached 1,500 lines with three designs interleaved. What a streamer
/// drops into OBS is still one URL; the browser just makes six requests instead of one.
pub const PAGE: &str = include_str!("page.html");

/// The rest of the page, in the order the shell links it: the shared base, one stylesheet per
/// overlay, and the script.
///
/// Embedded the same way the page is, and looked up **by name in this table** rather than used to
/// build a path. A request can therefore only ever name something that exists here: the filesystem
/// is never consulted, so there is nothing to traverse. `/../page.html` and `/etc/passwd` are both
/// simply not in the list.
///
/// Every overlay's sheet is served to all six. Their rules are all scoped by `body[data-design=…]`,
/// so an unused one matches nothing — cheaper than loading it after the first paint, and it means a
/// rule cannot physically leak from one overlay into another.
///
/// A design is one file per overlay rather than one per design family, which costs **153 duplicated
/// lines of code** — the two orientations of a design still share its artwork keyframes and, for
/// vinyl, its whole disc treatment. `shared_rules_are_identical_across_an_overlays_pair` is what
/// stops the two copies drifting apart, because that is the one thing the duplication risks.
pub const ASSETS: &[(&str, &str, &str)] = &[
    ("base.css", include_str!("base.css"), "text/css; charset=utf-8"),
    ("designs/sleeve-wide.css", include_str!("designs/sleeve-wide.css"), "text/css; charset=utf-8"),
    ("designs/sleeve-tall.css", include_str!("designs/sleeve-tall.css"), "text/css; charset=utf-8"),
    (
        "designs/playout-wide.css",
        include_str!("designs/playout-wide.css"),
        "text/css; charset=utf-8",
    ),
    (
        "designs/playout-tall.css",
        include_str!("designs/playout-tall.css"),
        "text/css; charset=utf-8",
    ),
    ("designs/vinyl-wide.css", include_str!("designs/vinyl-wide.css"), "text/css; charset=utf-8"),
    ("designs/vinyl-tall.css", include_str!("designs/vinyl-tall.css"), "text/css; charset=utf-8"),
    ("overlay.js", include_str!("overlay.js"), "text/javascript; charset=utf-8"),
];

/// The entry for `name`, if it is one of ours.
pub fn asset(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    ASSETS.iter().find(|(n, _, _)| *n == name).copied()
}

#[derive(Debug, PartialEq)]
pub enum Route {
    Page,
    /// One of the stylesheets or the script the shell links to. Carries the name so the handler does
    /// not have to parse the path a second time — and so it can only be a name from [`ASSETS`].
    Asset(&'static str),
    State,
    /// `POST` with `{"action": "…"}`. What makes the overlay's transport buttons real.
    Control,
    /// Artwork, fetched by us and handed to the overlay. See [`cover_allowed`].
    Cover,
    /// `/token` with no trailing slash. Redirected rather than served, so the page's own relative
    /// URLs and the browser's idea of the base stay in agreement.
    Redirect,
}

/// Split a request path into a route, refusing anything whose token does not match.
///
/// Pure, so the token check — the only thing standing between a local process and the endpoint — is
/// testable without opening a socket.
pub fn route(path: &str, token: &str) -> Option<Route> {
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
                // Anything else has to be a name in ASSETS, which is what makes the lookup safe.
                other => {
                    ASSETS.iter().find(|(n, _, _)| *n == other).map(|(n, _, _)| Route::Asset(n))
                }
            }
        }
    }
}

/// Turn what YouTube hands us into something fetchable, or `None` if it is unusable.
///
/// InnerTube returns thumbnails as absolute `https://` URLs today, but it also returns
/// **protocol-relative** ones (`//lh3.googleusercontent.com/…`) and has for years. Handed to an
/// `Image` from a `file://` page, `//host/path` resolves to `file://host/path` and fails with
/// nothing a page can report — which is exactly what "the cover does not load" looks like.
pub fn normalize_cover(raw: &str) -> Option<String> {
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
pub fn cover_allowed(url: &str) -> Option<reqwest::Url> {
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
pub enum Action {
    Prev,
    Next,
    Toggle,
}

impl Action {
    pub fn parse(raw: &str) -> Option<Action> {
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
pub const MAX_CONTROL_BYTES: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "abc123def456abc1";

    /// Every part of the page, as one string. The assertions below are about the page as the browser
    /// sees it, which since the split is the shell plus everything it links.
    fn whole_page() -> String {
        let mut s = String::from(PAGE);
        for (_, body, _) in ASSETS {
            s.push('\n');
            s.push_str(body);
        }
        s
    }

    /// The assets are served by looking the requested name up in [`ASSETS`], never by building a
    /// path from it. That is what makes the route safe without any path handling at all: the
    /// filesystem is never consulted, so `..` has nothing to climb.
    #[test]
    fn assets_route_by_name_and_nothing_else() {
        for (name, body, ctype) in ASSETS {
            assert_eq!(
                route(&format!("/{T}/{name}"), T),
                Some(Route::Asset(name)),
                "{name} does not route"
            );
            assert!(!body.is_empty(), "{name} is embedded empty");
            assert!(
                ctype.starts_with("text/css") || ctype.starts_with("text/javascript"),
                "{name} has a type a browser will refuse"
            );
            // And the shell has to actually link it, or the file ships and nobody loads it.
            assert!(PAGE.contains(name), "the shell never links {name}");
        }

        // Nothing outside the table is reachable, however it is spelled. The first two only prove
        // the point because the lookup is by name: with a path they would be the interesting cases.
        for bad in [
            "../page.html",
            "base.css/../overlay.js",
            "..%2fpage.html",
            "main.rs",
            "Cargo.toml",
            "base.css.bak",
            "BASECSS",
        ] {
            assert_eq!(route(&format!("/{T}/{bad}"), T), None, "{bad} must not route");
        }

        // The token still gates them, as it gates everything else.
        assert_eq!(route("/wrongtoken000000/base.css", T), None);
    }

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

    #[test]
    fn the_control_endpoint_routes() {
        assert_eq!(route(&format!("/{T}/control"), T), Some(Route::Control));
        // And it is behind the same token as everything else.
        assert_eq!(route("/wrongtoken000000/control", T), None);
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

    /// The page is shipped from the binary, so a missing or truncated file is a build-time problem
    /// that would otherwise only show up as a blank overlay on stream. Pin what it needs.
    ///
    /// Checked over the whole set, not the shell. Since the split the shell is a hundred lines of
    /// markup, and it would pass a size check on its own while every rule it depends on was missing.
    #[test]
    fn the_embedded_page_is_intact() {
        let all = whole_page();
        assert!(all.len() > 60_000, "the page looks truncated: {} bytes", all.len());
        // All three designs must be reachable by the query parameter the Settings panel sends.
        for design in ["sleeve", "playout", "vinyl"] {
            assert!(all.contains(design), "the page has no {design} design");
        }
        // The four knobs the link exposes, and the two properties that make it an overlay at all.
        for needle in ["design", "pos", "scale", "demo"] {
            assert!(all.contains(needle), "the page does not read {needle}");
        }
        assert!(all.contains("background: transparent"), "the overlay must not paint a backdrop");
        assert!(all.contains("prefers-reduced-motion"), "motion must be suppressed on request");
        // Polling has to be built from the page's own path, because the endpoint lives under the
        // token (`/<token>/state`) — a bare `/state` fetch silently 404s and the overlay stays blank.
        assert!(
            all.contains(r#"location.pathname.replace"#),
            "the state URL must be derived from the page's own path"
        );
        assert!(!all.contains(r#"fetch("/state""#), "a bare /state fetch would miss the token");
    }

    /// Every stylesheet has to be linked by the shell, and the shell is not allowed to link anything
    /// that is not in [`ASSETS`] — otherwise a rename leaves a 404 in OBS and nothing says so.
    #[test]
    fn the_shell_and_the_table_agree() {
        for (name, _, _) in ASSETS {
            assert!(
                PAGE.contains(&format!(r#"href="{name}""#))
                    || PAGE.contains(&format!(r#"src="{name}""#)),
                "the shell does not link {name}"
            );
        }
        for line in PAGE.lines() {
            for attr in ["href=\"", "src=\""] {
                let Some(at) = line.find(attr) else { continue };
                let rest = &line[at + attr.len()..];
                let Some(end) = rest.find('"') else { continue };
                let target = &rest[..end];
                if target.starts_with("http") || target.starts_with("data:") {
                    continue;
                }
                assert!(
                    asset(target).is_some(),
                    "the shell links {target:?}, which is not in ASSETS and would 404"
                );
            }
        }
    }

    /// The top-level rules of a stylesheet as `(selector, body)`, with comments out and everything
    /// nested discarded.
    ///
    /// Only depth zero matters: `@keyframes` steps (`0%`, `from`, `to`) live one level down and are
    /// not selectors. Counting them was the false positive that made this look like it was failing
    /// when it was not.
    fn rules(css: &str) -> Vec<(String, String)> {
        let chars: Vec<char> = css.chars().collect();
        let mut clean = String::with_capacity(css.len());
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
                continue;
            }
            clean.push(chars[i]);
            i += 1;
        }

        let mut out = Vec::new();
        let mut buf = String::new();
        let mut depth = 0i32;
        let mut start = 0usize;
        let bytes: Vec<char> = clean.chars().collect();
        for (at, c) in bytes.iter().enumerate() {
            match c {
                '{' => {
                    if depth == 0 {
                        // Whitespace inside the selector is collapsed, not just trimmed. Two rules
                        // saying the same thing with a line break in different places are the same
                        // rule, and a comparison that thinks otherwise fails on reformatting — which
                        // is exactly what happened when one of a pair was reflowed for readability.
                        let sel = buf.split_whitespace().collect::<Vec<_>>().join(" ");
                        // An at-rule is not a selector, and nothing inside one sits at depth zero.
                        if !sel.is_empty() && !sel.starts_with('@') {
                            start = at;
                            out.push((sel, String::new()));
                        }
                    }
                    depth += 1;
                    buf.clear();
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(last) = out.last_mut() {
                            if last.1.is_empty() {
                                last.1 = bytes[start..=at].iter().collect();
                            }
                        }
                    }
                    buf.clear();
                }
                _ => {
                    if depth == 0 {
                        buf.push(*c);
                    }
                }
            }
        }
        out.retain(|(_, body)| !body.is_empty());
        out
    }

    fn selectors(css: &str) -> Vec<String> {
        rules(css).into_iter().map(|(s, _)| s).collect()
    }

    /// A design stylesheet may only contain rules scoped to its own design.
    ///
    /// All three are linked by all six overlays, so an unscoped rule in one of them applies to every
    /// overlay. That is not hypothetical: the `@media (prefers-reduced-motion: reduce)` block is
    /// global, it sat at the end of the original single file, and the split put it in the last
    /// design's sheet — so reduced-motion support for all six overlays was being provided by
    /// `vinyl.css`. It worked, and would have kept working right up until someone dropped that link.
    ///
    /// This is the check that finds that class of mistake. It found the one above.
    #[test]
    fn a_design_sheet_only_contains_its_own_rules() {
        for (name, body, _) in ASSETS {
            if !name.ends_with(".css") || *name == "base.css" {
                continue;
            }
            for sel in selectors(body) {
                assert!(
                    sel.contains("data-design"),
                    "{name} has a rule that is not scoped to a design, so it would apply to every \
                     overlay: {sel}"
                );
            }
            // And nothing global may live here, however it is spelled.
            for global in ["prefers-reduced-motion", ":root", "html, body", "*, *::before"] {
                assert!(
                    !body.contains(global),
                    "{name} carries `{global}`, which is shared and belongs in base.css"
                );
            }
        }
        // The base is where those belong, so it has to actually have them.
        let base = ASSETS.iter().find(|(n, _, _)| *n == "base.css").unwrap().1;
        assert!(base.contains("prefers-reduced-motion"), "the base must carry the motion query");
        assert!(base.contains(":root"), "the base must carry the tokens");
    }

    /// The two orientations of a design share rules, and one file per overlay duplicated them.
    ///
    /// This is the guard for the one real risk the split introduced. 153 lines of code now exist
    /// twice: the artwork keyframes each pair has in common, and for vinyl its whole disc treatment.
    /// A change to one copy and not the other would leave two overlays that are supposed to differ
    /// only in composition quietly diverging — and nothing else in the project would notice.
    ///
    /// Compares the rule *bodies*, not just the selectors: two copies that drifted would still match
    /// on the selector while the declarations inside said different things.
    #[test]
    fn shared_rules_are_identical_across_an_overlays_pair() {
        for (family, wide, tall) in [
            ("sleeve", "designs/sleeve-wide.css", "designs/sleeve-tall.css"),
            ("playout", "designs/playout-wide.css", "designs/playout-tall.css"),
            ("vinyl", "designs/vinyl-wide.css", "designs/vinyl-tall.css"),
        ] {
            let a = asset(wide).expect("wide sheet").1;
            let b = asset(tall).expect("tall sheet").1;
            let marker = format!(r#"^="{family}""#);

            // A rule scoped to the whole family has to be in both files, identically.
            let shared: Vec<(String, String)> =
                rules(a).into_iter().filter(|(s, _)| s.contains(&marker)).collect();
            assert!(
                !shared.is_empty(),
                "{family} shares nothing between its orientations — the split went wrong"
            );
            for (sel, body) in shared {
                let other = rules(b).into_iter().find(|(s, _)| *s == sel).unwrap_or_else(|| {
                    panic!("{family}: `{sel}` is shared but missing from {tall}")
                });
                assert_eq!(
                    normalize(&body),
                    normalize(&other.1),
                    "{family}: `{sel}` has drifted — {wide} and {tall} no longer say the same thing"
                );
            }

            // And neither file may carry a rule for the other orientation.
            for (name, own, other_side) in [(wide, "wide", "tall"), (tall, "tall", "wide")] {
                let foreign = format!(r#"body[data-design="{family}-{other_side}"]"#);
                assert!(
                    !asset(name).expect("sheet").1.contains(&foreign),
                    "{name} carries a rule for {family}-{other_side}, which is not its overlay"
                );
                let _ = own;
            }
        }
    }

    /// A rule body reduced to what it *says*, so reformatting is not mistaken for drift.
    ///
    /// The first version of this compared the raw text of the body, and the moment one of a pair was
    /// reflowed for readability the guard failed — on a difference of line breaks, in a rule whose
    /// declarations were identical. A check that cries wolf about whitespace is a check somebody
    /// eventually silences, and then it is not checking anything.
    ///
    /// Declarations are compared as a multiset, because the cascade does not care about their order
    /// either — two rules with the same declarations in a different order say the same thing.
    fn normalize(body: &str) -> Vec<String> {
        let inner = body
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .to_string();
        let mut out: Vec<String> = inner
            .split(';')
            .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|d| !d.is_empty())
            .collect();
        out.sort();
        out
    }

    /// The typography declarations of a rule body.
    fn typography(body: &str) -> Vec<String> {
        const PROPS: &[&str] = &[
            "font-family",
            "font-size",
            "font-weight",
            "letter-spacing",
            "line-height",
            "text-align",
            "text-transform",
            "text-shadow",
            "font-variant-numeric",
        ];
        body.split(';')
            .map(str::trim)
            .filter(|d| {
                let lower = d.to_ascii_lowercase();
                PROPS.iter().any(|p| lower.starts_with(p))
            })
            .map(str::to_string)
            .collect()
    }

    /// Type that is the same in both orientations of a design is shared type, and belongs in one
    /// rule with a family selector.
    ///
    /// This is the shape the check above cannot see. Playout's typography was written twice, once
    /// per orientation, with exact selectors — and by the time anyone looked the copies had already
    /// drifted: the wide title carried `letter-spacing: 0.01em` and the tall one never got it. Two
    /// exact-selector copies are invisible to `shared_rules_are_identical_across_an_overlays_pair`,
    /// because that compares family-selector rules — the shape the split actually duplicates.
    ///
    /// A declaration that is identical on both sides has to become one shared rule. What genuinely
    /// differs between two orientations — playout's 28px title against its 26px one — is left alone,
    /// and that difference is the reason this check is about individual declarations rather than
    /// whole rules.
    #[test]
    fn type_shared_by_two_orientations_is_written_once() {
        for (family, wide, tall) in [
            ("sleeve", "designs/sleeve-wide.css", "designs/sleeve-tall.css"),
            ("playout", "designs/playout-wide.css", "designs/playout-tall.css"),
            ("vinyl", "designs/vinyl-wide.css", "designs/vinyl-tall.css"),
        ] {
            let a = asset(wide).expect("wide sheet").1;
            let b = asset(tall).expect("tall sheet").1;
            let mut compared = 0;

            for (sel, body) in rules(a) {
                let marker = format!(r#""{family}-wide""#);
                if !sel.contains(&marker) {
                    continue;
                }
                let mirror = sel.replace(&format!("{family}-wide"), &format!("{family}-tall"));
                let Some((_, other)) = rules(b).into_iter().find(|(s, _)| *s == mirror) else {
                    continue;
                };
                compared += 1;
                let mine = typography(&body);
                for shared in typography(&other) {
                    assert!(
                        !mine.contains(&shared),
                        "{family}: `{shared}` is written under both orientation selectors, so it \
                         is shared type that belongs in one `^=\"{family}\"` rule — two copies are \
                         how the wide title quietly gained tracking the tall one never got"
                    );
                }
            }
            assert!(compared > 0, "{family}: no paired rules found — did the naming change?");
        }
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
}
