//! What a web page in somebody's browser is allowed to ask this server.
//!
//! The server listens on loopback and, by default, asks nobody for a token,
//! on the reasoning that the port is the boundary. It is not the whole
//! boundary. Every page the person running it opens in a browser can also send
//! requests to `127.0.0.1:8080`, and two kinds of request need no permission
//! from anyone:
//!
//! * **A cross-site write.** A `POST` with a `text/plain` body is a "simple"
//!   request: the browser sends it without asking the server first, and only
//!   hides the *response* from the page. `/hook` reads its body as a string
//!   and never looked at the content type, so a page could record a prompt
//!   into memory — and memory is what the next session is handed. A plain
//!   `<img src="…/handoff?…">` spends the note the next session was owed.
//! * **A rebound read.** A page on a domain whose DNS answer is switched to
//!   `127.0.0.1` after it loads is, to the browser, still on its own origin,
//!   and may read whatever that origin serves — `/api/v1` and `/ui` are the
//!   whole of memory.
//!
//! Both are closed here, by facts only a browser ever sends, so nothing that is
//! not a browser — a hook, `anamnesis status`, a script with `curl` — sees any
//! difference:
//!
//! * `Sec-Fetch-Site` says whether a request came from this server's own
//!   pages. Every current browser sends it. Where it is missing, `Origin` is
//!   compared with `Host`, which is what older browsers send on a write.
//! * `Host` names the address the browser *thinks* it is talking to. On a
//!   loopback server that nobody needs a token for, a request naming any host
//!   other than a loopback one came through a name that was rebound.
//!
//! The host rule stands down when tokens are required, and for a reason worth
//! keeping: the documented way to share a server puts it on loopback behind a
//! proxy that forwards the public name as `Host`. There, a rebound page holds
//! no token and gets a 401 from the guard, so refusing the name would break the
//! deployment and protect nothing.

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Refuse requests a page on another site made on its own.
///
/// Three answers, in order:
///
/// 1. `Sec-Fetch-Site: same-origin` or `none` — this server's own pages, or a
///    person typing an address — goes through.
/// 2. Any other `Sec-Fetch-Site` goes through only as a person following a link
///    to the wiki browser: a top-level navigation to a page under `/ui`. That
///    is a read whose result only the person sees. The same navigation to
///    `/handoff` is refused, because opening that address *is* the write.
/// 3. With no fetch metadata, an `Origin` that does not name this host is
///    refused. No `Origin` at all is a client that is not a browser.
pub(crate) async fn refuse_cross_site(request: Request, next: Next) -> Response {
    match cross_site_verdict(request.method(), request.uri().path(), request.headers()) {
        Verdict::Allow => next.run(request).await,
        Verdict::Refuse(reason) => {
            tracing::warn!(
                path = %request.uri().path(),
                reason,
                "refused a request a page on another site made"
            );
            refusal(reason)
        }
    }
}

/// Refuse requests that name a host this loopback server does not answer to.
///
/// Installed by [`crate::app`] only when the server is bound to loopback and
/// accepts requests without a token — see the module documentation for why
/// both conditions matter.
pub(crate) async fn refuse_foreign_host(request: Request, next: Next) -> Response {
    let named = requested_host(&request);
    match named.as_deref() {
        // HTTP/1.0 sends no host at all, and no browser speaks it.
        None => next.run(request).await,
        Some(host) if is_loopback_host(host) => next.run(request).await,
        Some(host) => {
            tracing::warn!(
                host,
                path = %request.uri().path(),
                "refused a request for a host this loopback server does not answer to"
            );
            refusal(
                "this server answers on its loopback address, not on the name this request used",
            )
        }
    }
}

/// Headers every response carries.
///
/// The wiki browser renders pages written by models and by capture, which is
/// to say untrusted text; raw HTML in them is already shown as text rather
/// than passed through. These are the second line behind that, and cost a
/// response nothing:
///
/// * a policy under which no script runs, no page is framed, and nothing loads
///   from anywhere but this server — a remote image in a page is a request that
///   tells somebody else which page was read, and when;
/// * no `Referer` when a link in a page is followed, since the path it would
///   carry names the project and the page;
/// * no content sniffing, so a JSON body is never reinterpreted as HTML.
pub(crate) async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

/// The policy [`security_headers`] sends.
///
/// `style-src 'unsafe-inline'` because every style the browser uses is inline,
/// on purpose — the binary is copied somewhere on its own and has no directory
/// of assets beside it. `form-action 'self'` for the search box.
pub(crate) const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; \
     img-src 'self' data:; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

/// What [`refuse_cross_site`] decided, and why, for the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Allow,
    Refuse(&'static str),
}

/// The decision itself, without a request to carry.
fn cross_site_verdict(method: &Method, path: &str, headers: &HeaderMap) -> Verdict {
    if let Some(site) = header_str(headers, "sec-fetch-site") {
        return match site.to_ascii_lowercase().as_str() {
            "same-origin" | "none" => Verdict::Allow,
            _ if is_link_to_the_browser(method, path, headers) => Verdict::Allow,
            _ => Verdict::Refuse("a page on another site cannot use this server"),
        };
    }

    match header_str(headers, "origin") {
        None => Verdict::Allow,
        Some(origin) => match (host_of_origin(origin), header_str(headers, "host")) {
            (Some(from), Some(host)) if from.eq_ignore_ascii_case(host) => Verdict::Allow,
            _ => Verdict::Refuse("a page on another site cannot use this server"),
        },
    }
}

/// A person clicking a link to the wiki browser somewhere else.
fn is_link_to_the_browser(method: &Method, path: &str, headers: &HeaderMap) -> bool {
    let navigating = header_str(headers, "sec-fetch-mode")
        .is_some_and(|mode| mode.eq_ignore_ascii_case("navigate"))
        && header_str(headers, "sec-fetch-dest")
            .is_some_and(|dest| dest.eq_ignore_ascii_case("document"));
    let reading = method == Method::GET || method == Method::HEAD;
    let browser = path == crate::ui::PREFIX || path.starts_with(&format!("{}/", crate::ui::PREFIX));
    navigating && reading && browser
}

/// `host[:port]` from an `Origin` value, or `None` for `null` and anything
/// that is not `scheme://authority`.
fn host_of_origin(origin: &str) -> Option<&str> {
    let (_, authority) = origin.split_once("://")?;
    let authority = authority.trim_end_matches('/');
    (!authority.is_empty() && !authority.contains('/')).then_some(authority)
}

/// The host a request names, from `Host` or, for HTTP/2, the URI.
fn requested_host(request: &Request) -> Option<String> {
    header_str(request.headers(), "host")
        .map(str::to_owned)
        .or_else(|| {
            request
                .uri()
                .authority()
                .map(|authority| authority.to_string())
        })
}

/// Whether `host` — as it appears in a `Host` header, port and all — names this
/// machine's loopback interface.
///
/// `localhost`, any literal in `127.0.0.0/8`, and `::1`. Deliberately not
/// `0.0.0.0`: no client of this server writes it, and browsers have been known
/// to route it to loopback for pages that could not otherwise reach it.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    let name = strip_port(host.trim());
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let literal = name
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(name);
    literal
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// `host` without its `:port`, leaving an IPv6 literal's own colons alone.
fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        return match host.find(']') {
            Some(end) => &host[..=end],
            None => host,
        };
    }
    match host.rsplit_once(':') {
        // One colon is a port; more than one is an unbracketed IPv6 literal.
        Some((name, port))
            if !name.contains(':')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => host,
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A 403 that says what happened, for whoever is looking at the network tab.
fn refusal(reason: &'static str) -> Response {
    (StatusCode::FORBIDDEN, format!("{reason}\n")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn a_client_that_is_not_a_browser_sends_nothing_and_is_let_through() {
        assert_eq!(
            cross_site_verdict(
                &Method::POST,
                "/hook",
                &headers(&[("host", "127.0.0.1:8080")])
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn fetch_metadata_decides_before_origin_is_looked_at() {
        let own = headers(&[
            ("sec-fetch-site", "same-origin"),
            ("origin", "http://elsewhere.example"),
            ("host", "127.0.0.1:8080"),
        ]);
        assert_eq!(
            cross_site_verdict(&Method::GET, "/api/v1/scopes", &own),
            Verdict::Allow
        );

        for site in ["cross-site", "same-site", "CROSS-SITE"] {
            let foreign = headers(&[("sec-fetch-site", site), ("host", "127.0.0.1:8080")]);
            assert!(
                matches!(
                    cross_site_verdict(&Method::POST, "/hook", &foreign),
                    Verdict::Refuse(_)
                ),
                "{site} must be refused"
            );
        }
    }

    /// `same-site` is refused as firmly as `cross-site`: another port on
    /// localhost is another site's server, and a development server somebody
    /// is running is exactly the kind of page that should not reach memory.
    #[test]
    fn another_port_on_this_machine_is_another_site() {
        let sibling = headers(&[("sec-fetch-site", "same-site"), ("host", "127.0.0.1:8080")]);
        assert!(matches!(
            cross_site_verdict(&Method::GET, "/handoff", &sibling),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn following_a_link_reaches_the_browser_and_nothing_else() {
        let link = headers(&[
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "document"),
        ]);
        assert_eq!(
            cross_site_verdict(&Method::GET, "/ui", &link),
            Verdict::Allow
        );
        assert_eq!(
            cross_site_verdict(&Method::GET, "/ui/default/widget", &link),
            Verdict::Allow
        );

        // Opening this address is the write, so arriving by link is refused.
        assert!(matches!(
            cross_site_verdict(&Method::GET, "/handoff", &link),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            cross_site_verdict(&Method::GET, "/api/v1/scopes", &link),
            Verdict::Refuse(_)
        ));
        // A prefix is not a path: `/uix` is not the browser.
        assert!(matches!(
            cross_site_verdict(&Method::GET, "/uix", &link),
            Verdict::Refuse(_)
        ));
        // And a form posted from elsewhere is not a link.
        assert!(matches!(
            cross_site_verdict(&Method::POST, "/ui", &link),
            Verdict::Refuse(_)
        ));

        let framed = headers(&[
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "navigate"),
            ("sec-fetch-dest", "iframe"),
        ]);
        assert!(matches!(
            cross_site_verdict(&Method::GET, "/ui", &framed),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn without_fetch_metadata_origin_has_to_name_this_host() {
        let same = headers(&[
            ("origin", "http://127.0.0.1:8080"),
            ("host", "127.0.0.1:8080"),
        ]);
        assert_eq!(
            cross_site_verdict(&Method::POST, "/hook", &same),
            Verdict::Allow
        );

        // Behind a TLS proxy the scheme differs from what the server sees, and
        // the host is what identifies the site.
        let proxied = headers(&[
            ("origin", "https://memory.example.com"),
            ("host", "memory.example.com"),
        ]);
        assert_eq!(
            cross_site_verdict(&Method::POST, "/hook", &proxied),
            Verdict::Allow
        );

        for origin in [
            "http://evil.example",
            "null",
            "http://127.0.0.1:9999",
            "garbage",
        ] {
            let foreign = headers(&[("origin", origin), ("host", "127.0.0.1:8080")]);
            assert!(
                matches!(
                    cross_site_verdict(&Method::POST, "/hook", &foreign),
                    Verdict::Refuse(_)
                ),
                "{origin} must be refused"
            );
        }
    }

    #[test]
    fn loopback_hosts_are_recognised_with_and_without_ports() {
        for host in [
            "localhost",
            "LOCALHOST:8080",
            "127.0.0.1",
            "127.0.0.1:8080",
            "127.4.5.6:80",
            "[::1]",
            "[::1]:8080",
            "::1",
        ] {
            assert!(is_loopback_host(host), "{host} is loopback");
        }
    }

    /// The names a rebinding attack arrives under, and the addresses a browser
    /// might be talked into treating as this machine.
    #[test]
    fn every_other_host_is_foreign() {
        for host in [
            "evil.example",
            "evil.example:8080",
            "127.0.0.1.evil.example",
            "localhost.evil.example",
            "0.0.0.0:8080",
            "192.168.1.10:8080",
            "[::ffff:127.0.0.1]:8080",
            "[::2]:8080",
            "",
            "[::1",
        ] {
            assert!(!is_loopback_host(host), "{host:?} is not loopback");
        }
    }

    #[test]
    fn an_origin_is_reduced_to_its_authority() {
        assert_eq!(
            host_of_origin("http://127.0.0.1:8080"),
            Some("127.0.0.1:8080")
        );
        assert_eq!(
            host_of_origin("https://memory.example.com"),
            Some("memory.example.com")
        );
        assert_eq!(host_of_origin("null"), None);
        assert_eq!(host_of_origin("http://"), None);
        assert_eq!(host_of_origin("http://a/b"), None);
    }
}
