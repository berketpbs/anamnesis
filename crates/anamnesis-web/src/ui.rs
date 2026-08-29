//! A read-only front door onto the wiki.
//!
//! Until this existed, the only ways to read memory were `anamnesis search`,
//! `anamnesis show-page`, and whatever an agent asked for over MCP — three
//! interfaces that all require knowing what to ask for. A wiki nobody can
//! *look at* is judged entirely by what it hands back, which is how a stale
//! page, a summary the model wrote badly, and a page that never got indexed
//! all look identical from the outside.
//!
//! Three things this surface deliberately does not do:
//!
//! * **It never writes.** Every route is a `GET`, and the browser is given no
//!   way to change what it is reading. Editing belongs to the wiki itself,
//!   where git records who changed what.
//! * **It does not record access.** `query_pages` bumps a page's access
//!   statistics because retrieval finding a page useful is evidence about the
//!   page; a person clicking through an index is not the same claim, and the
//!   decay sweep reads exactly those counters. Browsing the wiki must not be
//!   able to rescue a page from being forgotten — `anamnesis show-page` has
//!   always taken the same position.
//! * **It does not trust the page.** Bodies here are written by models, by
//!   consolidation, and by whoever edits the wiki in an editor. Raw HTML is
//!   rendered as the text it is, and link destinations that are not http(s)
//!   or mailto are defused.

use std::collections::HashMap;

use anamnesis_core::page::{Frontmatter, PagePath};
use anamnesis_core::scope::Scope;
use anamnesis_store::ProjectRow;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Router, middleware};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html::push_html};

use crate::AppState;

/// Where the browser lives. One prefix, so the API keeps every path it had and
/// a proxy in front of the server can route the two apart.
pub const PREFIX: &str = "/ui";

/// The realm a browser shows in its credential prompt.
const REALM: &str = "anamnesis";

/// Routes for the browsable wiki, behind the guard a browser can satisfy.
pub(crate) fn routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route(PREFIX, get(index))
        .route(&format!("{PREFIX}/{{workspace}}/{{project}}"), get(scope))
        .route(
            &format!("{PREFIX}/{{workspace}}/{{project}}/{{*path}}"),
            get(page),
        )
        // `route_layer`, for the same reason the API uses it: a path this
        // server does not serve is a 404, not a prompt for credentials.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            super::require_browser_token,
        ))
}

/// What went wrong, in a shape a browser can render.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UiError {
    /// No such scope, or no such page in it.
    #[error("{0}")]
    Missing(String),

    /// The index could not be read.
    #[error("storage error: {0}")]
    Store(#[from] anamnesis_store::StoreError),

    /// The wiki could not be read.
    #[error("wiki error: {0}")]
    Wiki(#[from] anamnesis_wiki::WikiError),

    /// The URL named something that is not a page path.
    #[error("{0}")]
    Core(#[from] anamnesis_core::CoreError),
}

impl IntoResponse for UiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Missing(_) => StatusCode::NOT_FOUND,
            Self::Core(_) => StatusCode::BAD_REQUEST,
            Self::Store(_) | Self::Wiki(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = shell(
            "Not here",
            &format!(
                "<h1>{}</h1><p class=\"muted\">{}</p><p><a href=\"{PREFIX}\">Back to the wiki</a></p>",
                status.as_u16(),
                escape(&self.to_string())
            ),
        );
        // `Html`, not a bare string: a body of markup served as `text/plain`
        // is shown to the reader as its own source.
        (status, Html(body)).into_response()
    }
}

/// The page a browser gets when it has not presented an accepted token.
///
/// Sent with `WWW-Authenticate: Basic`, which is the whole reason the UI
/// accepts that scheme: a browser cannot be talked into attaching a bearer
/// token to a link someone clicked, but it will ask for a password and then
/// send it. Any username is accepted — the secret is the entire credential,
/// and a username that had to match would only add a failure mode whose error
/// message cannot say which half was wrong.
pub(crate) fn challenge(reason: &str) -> Response {
    let body = shell(
        "Sign in",
        &format!(
            "<h1>This memory is private</h1>\
             <p>{}</p>\
             <p class=\"muted\">Your browser will ask for a username and a password. \
             Any username will do; the password is the token this server accepts \
             (the value of <code>ANAMNESIS_TOKEN</code> on a machine it trusts).</p>",
            escape(reason)
        ),
    );
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            format!("Basic realm=\"{REALM}\", charset=\"UTF-8\""),
        )],
        Html(body),
    )
        .into_response()
}

/// Every scope this server holds memory for.
async fn index(State(state): State<AppState>) -> Result<Html<String>, UiError> {
    let projects = state.store.projects()?;

    let mut rows = String::new();
    for project in &projects {
        let pages = state.store.page_count(project.project_id)?;
        rows.push_str(&format!(
            "<tr><td><a href=\"{href}\">{workspace}/{project_name}</a></td>\
             <td class=\"num\">{pages}</td></tr>",
            href = escape(&scope_href(&project.scope)),
            workspace = escape(project.scope.workspace.as_str()),
            project_name = escape(project.scope.project.as_str()),
        ));
    }

    let body = if projects.is_empty() {
        // Empty is the normal state of a server nobody has run hooks against
        // yet, and saying so beats an empty table that reads like a fault.
        "<h1>anamnesis</h1><p class=\"muted\">No project has been registered here yet. \
         A scope appears once a session is captured for it, or once \
         <code>anamnesis init</code> runs inside a repository.</p>"
            .to_owned()
    } else {
        format!(
            "<h1>anamnesis</h1>\
             <table><thead><tr><th>Scope</th><th class=\"num\">Pages</th></tr></thead>\
             <tbody>{rows}</tbody></table>"
        )
    };

    Ok(Html(shell("anamnesis", &body)))
}

/// Every page in one scope.
async fn scope(
    State(state): State<AppState>,
    Path((workspace, project)): Path<(String, String)>,
) -> Result<Html<String>, UiError> {
    let found = find_scope(&state, &workspace, &project)?;

    let mut pages = state.store.sweep_rows(found.project_id)?;
    // Newest first: an index of a memory is read to see what it has learned
    // lately far more often than to find something alphabetically. The path
    // tiebreak keeps two pages written in the same second in a stable order.
    pages.sort_by(|a, b| {
        b.facts
            .written_at
            .cmp(&a.facts.written_at)
            .then_with(|| a.path.as_str().cmp(b.path.as_str()))
    });

    let base = scope_href(&found.scope);
    let mut rows = String::new();
    for row in &pages {
        rows.push_str(&format!(
            "<tr><td><a href=\"{href}\">{title}</a><div class=\"path\">{path}</div></td>\
             <td>{badges}</td>\
             <td class=\"num\">{written}</td>\
             <td class=\"num\">{reads}</td></tr>",
            href = escape(&format!("{base}/{}", encode_path(row.path.as_str()))),
            title = escape(&row.title),
            path = escape(row.path.as_str()),
            badges = badges(
                row.facts.tier.as_str(),
                row.facts.status.as_str(),
                row.facts.pinned,
                row.facts.canonical,
            ),
            written = row.facts.written_at.strftime("%Y-%m-%d"),
            reads = row.facts.access_count,
        ));
    }

    let heading = format!(
        "<h1>{}/{}</h1>",
        escape(found.scope.workspace.as_str()),
        escape(found.scope.project.as_str())
    );

    let body = if pages.is_empty() {
        format!(
            "{heading}<p class=\"muted\">This scope has no pages in the index. \
             If the wiki has files in it, <code>anamnesis reindex</code> puts them back.</p>"
        )
    } else {
        format!(
            "{heading}\
             <table><thead><tr><th>Page</th><th></th>\
             <th class=\"num\">Written</th><th class=\"num\">Reads</th></tr></thead>\
             <tbody>{rows}</tbody></table>\
             <p class=\"muted\">{count} pages. Reads are what the decay sweep counts; \
             opening one here is not one of them.</p>",
            count = pages.len()
        )
    };

    let page = format!("{}{body}", crumbs(&[("anamnesis", PREFIX)]));
    Ok(Html(shell(&format!("{workspace}/{project}"), &page)))
}

/// One page, rendered.
async fn page(
    State(state): State<AppState>,
    Path((workspace, project, path)): Path<(String, String, String)>,
) -> Result<Html<String>, UiError> {
    let found = find_scope(&state, &workspace, &project)?;
    let page_path = PagePath::parse(&path)?;

    // Read from the wiki rather than from the index's copy of the body. The
    // file is what a person edits and what git holds; the index is a
    // derivative that can be a watcher tick — or a whole `reindex` — behind it.
    let (parsed, links) = {
        let wiki = state.wiki.lock();
        if !wiki.exists(&found.scope, &page_path) {
            return Err(UiError::Missing(format!(
                "the index has {path} in {workspace}/{project}, but there is no file for it — \
                 `anamnesis reindex` forgets pages the wiki no longer has"
            )));
        }
        let parsed = wiki.read_page(&found.scope, &page_path)?;
        let base = scope_href(&found.scope);
        let links: HashMap<String, Option<String>> = anamnesis_wiki::extract_links(&parsed.body)
            .into_iter()
            .map(|target| {
                let href = resolve_link(&wiki, &found.scope, &base, &target);
                (target, href)
            })
            .collect();
        (parsed, links)
    };

    // Worth saying loudly, and for the same reason `show-page` says it:
    // retrieval stopped offering this page the moment something replaced it,
    // so a reader who arrived here by name has no other way to learn that.
    let replaced = state.store.superseded_by(found.project_id, &page_path)?;

    let base = scope_href(&found.scope);
    let body = format!(
        "{crumbs}<h1>{title}</h1>\
         <div class=\"meta\">{badges}<span class=\"path\">{path}</span></div>\
         {notes}\
         <article>{rendered}</article>",
        crumbs = crumbs(&[
            ("anamnesis", PREFIX),
            (
                &format!("{}/{}", found.scope.workspace, found.scope.project),
                &base
            ),
        ]),
        title = escape(&parsed.frontmatter.title),
        badges = badges(
            parsed.frontmatter.tier.as_str(),
            parsed.frontmatter.status.as_str(),
            parsed.frontmatter.pinned,
            parsed.frontmatter.canonical,
        ),
        path = escape(page_path.as_str()),
        notes = notes(&parsed.frontmatter, replaced.as_ref(), &base),
        rendered = to_html(&parsed.body, &links),
    );

    Ok(Html(shell(&parsed.frontmatter.title, &body)))
}

/// The scope named by a URL, or a 404 that says what was looked for.
///
/// Matched against the registered projects by name rather than derived from
/// the URL: `_global` is a scope no `ProjectName::parse` will accept, and a
/// browser must be able to reach the shared pages retrieval already reads.
fn find_scope(state: &AppState, workspace: &str, project: &str) -> Result<ProjectRow, UiError> {
    state
        .store
        .projects()?
        .into_iter()
        .find(|row| {
            row.scope.workspace.as_str() == workspace && row.scope.project.as_str() == project
        })
        .ok_or_else(|| UiError::Missing(format!("no scope named {workspace}/{project} here")))
}

/// Where a `[[wiki link]]` points, if it points at a page that exists.
///
/// Both spellings the index resolves are accepted here too — `[[decisions]]`
/// and `[[decisions.md]]` name the same page — so what the browser shows as a
/// live link is what the link stream in retrieval sees as an edge.
fn resolve_link(
    wiki: &anamnesis_wiki::Wiki,
    scope: &Scope,
    base: &str,
    target: &str,
) -> Option<String> {
    for candidate in [target.to_owned(), format!("{target}.md")] {
        let Ok(path) = PagePath::parse(&candidate) else {
            continue;
        };
        if wiki.exists(scope, &path) {
            return Some(format!("{base}/{}", encode_path(path.as_str())));
        }
    }
    None
}

/// Frontmatter worth showing above the body.
fn notes(frontmatter: &Frontmatter, replaced: Option<&PagePath>, base: &str) -> String {
    let mut out = String::new();

    if let Some(replacement) = replaced {
        out.push_str(&format!(
            "<p class=\"warn\">Replaced by <a href=\"{href}\">{path}</a> — retrieval offers that \
             page instead of this one.</p>",
            href = escape(&format!("{base}/{}", encode_path(replacement.as_str()))),
            path = escape(replacement.as_str()),
        ));
    }
    if let Some(replaces) = &frontmatter.supersedes {
        out.push_str(&format!(
            "<p class=\"muted\">Replaces <a href=\"{href}\">{path}</a>.</p>",
            href = escape(&format!("{base}/{}", encode_path(replaces.as_str()))),
            path = escape(replaces.as_str()),
        ));
    }
    if let Some(expires) = frontmatter.expires_at {
        out.push_str(&format!(
            "<p class=\"muted\">Expires {}.</p>",
            expires.strftime("%Y-%m-%d")
        ));
    }
    if !frontmatter.entities.is_empty() {
        let names: Vec<String> = frontmatter
            .entities
            .iter()
            .map(|entity| format!("<span class=\"badge\">{}</span>", escape(entity.as_str())))
            .collect();
        out.push_str(&format!("<p class=\"meta\">{}</p>", names.join(" ")));
    }
    out
}

/// Tier, status, and the two flags that change what happens to a page.
fn badges(tier: &str, status: &str, pinned: bool, canonical: bool) -> String {
    let mut out = format!(
        "<span class=\"badge\">{}</span><span class=\"badge\">{}</span>",
        escape(tier),
        escape(status)
    );
    if pinned {
        out.push_str("<span class=\"badge pin\">pinned</span>");
    }
    if canonical {
        out.push_str("<span class=\"badge pin\">canonical</span>");
    }
    out
}

/// A trail back to where the reader came from.
fn crumbs(trail: &[(&str, &str)]) -> String {
    let links: Vec<String> = trail
        .iter()
        .map(|(label, href)| format!("<a href=\"{}\">{}</a>", escape(href), escape(label)))
        .collect();
    format!("<nav>{}</nav>", links.join(" / "))
}

/// The browser path for a scope.
fn scope_href(scope: &Scope) -> String {
    format!(
        "{PREFIX}/{}/{}",
        encode_path(scope.workspace.as_str()),
        encode_path(scope.project.as_str())
    )
}

/// Render a page body, with `[[wiki links]]` turned into links.
///
/// Everything the markdown says about HTML is deliberately disbelieved: a
/// body is untrusted input that happens to be stored on disk, written by a
/// model, by consolidation of somebody's prompts, or by hand.
fn to_html(body: &str, links: &HashMap<String, Option<String>>) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let mut events: Vec<Event<'_>> = Vec::new();
    let mut code = 0usize;
    // Text arrives in more pieces than it was written in: `[[a page]]` reaches
    // us as several events, because the inner brackets look like a reference
    // link that resolves to nothing. Runs of text are rejoined before wiki
    // links are looked for, or every one of them would be invisible here.
    let mut run = String::new();

    for event in Parser::new_ext(body, options) {
        match event {
            // Shown as text, not passed through. A page that quotes a `<script>`
            // tag is describing one; a page that carries one got it from a
            // prompt somebody's agent captured.
            Event::Html(raw) | Event::InlineHtml(raw) if code == 0 => run.push_str(&raw),
            Event::Html(raw) | Event::InlineHtml(raw) => events.push(Event::Text(raw)),

            Event::Text(text) if code == 0 => run.push_str(&text),

            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut run, links, &mut events);
                code += 1;
                events.push(Event::Start(Tag::CodeBlock(kind)));
            }
            Event::End(TagEnd::CodeBlock) => {
                code = code.saturating_sub(1);
                events.push(Event::End(TagEnd::CodeBlock));
            }

            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                flush(&mut run, links, &mut events);
                events.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url: safe_url(&dest_url),
                    title,
                    id,
                }));
            }
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                flush(&mut run, links, &mut events);
                events.push(Event::Start(Tag::Image {
                    link_type,
                    dest_url: safe_url(&dest_url),
                    title,
                    id,
                }));
            }

            other => {
                flush(&mut run, links, &mut events);
                events.push(other);
            }
        }
    }
    flush(&mut run, links, &mut events);

    let mut html = String::new();
    push_html(&mut html, events.into_iter());
    html
}

/// Emit a finished run of text, with its wiki links turned into links.
fn flush<'a>(
    run: &mut String,
    links: &HashMap<String, Option<String>>,
    events: &mut Vec<Event<'a>>,
) {
    if !run.is_empty() {
        expand(run, links, events);
        run.clear();
    }
}

/// Split text on `[[targets]]`, emitting a link for each one.
fn expand<'a>(text: &str, links: &HashMap<String, Option<String>>, events: &mut Vec<Event<'a>>) {
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let target = after[..end].trim();

        if target.is_empty() {
            // Not a link, just two brackets. Keep them as written.
            events.push(Event::Text(CowStr::from(
                rest[..start + 2 + end + 2].to_owned(),
            )));
            rest = &after[end + 2..];
            continue;
        }

        if start > 0 {
            events.push(Event::Text(CowStr::from(rest[..start].to_owned())));
        }
        match links.get(target).and_then(Option::as_ref) {
            Some(href) => {
                events.push(Event::Start(Tag::Link {
                    link_type: pulldown_cmark::LinkType::Inline,
                    dest_url: CowStr::from(href.clone()),
                    title: CowStr::from(""),
                    id: CowStr::from(""),
                }));
                events.push(Event::Text(CowStr::from(target.to_owned())));
                events.push(Event::End(TagEnd::Link));
            }
            // A link to a page nobody has written is worth seeing rather than
            // hiding: it is the same signal `improve` turns into a proposal
            // once two pages ask for the same missing target.
            None => events.push(Event::InlineHtml(CowStr::from(format!(
                "<span class=\"missing\" title=\"no page here yet\">{}</span>",
                escape(target)
            )))),
        }
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        events.push(Event::Text(CowStr::from(rest.to_owned())));
    }
}

/// A link destination the browser may follow, or `#` if it is not one.
///
/// Relative destinations and http(s)/mailto pass; everything else — `javascript:`
/// first among them — is replaced rather than dropped, so the text of the link
/// still reads as the page wrote it and clicking it does nothing.
fn safe_url(dest: &str) -> CowStr<'static> {
    let trimmed = dest.trim();
    let allowed = match trimmed.split_once(':') {
        // No colon at all: a relative link, a fragment, or a path.
        None => true,
        Some((scheme, _)) => {
            // A colon inside a path component is not a scheme: `notes/a:b`
            // and `#a:b` are relative, and a scheme cannot contain a slash.
            if scheme.contains(['/', '#', '?']) || scheme.is_empty() {
                true
            } else {
                matches!(
                    scheme.trim().to_ascii_lowercase().as_str(),
                    "http" | "https" | "mailto"
                )
            }
        }
    };

    if allowed {
        CowStr::from(trimmed.to_owned())
    } else {
        CowStr::from("#")
    }
}

/// Percent-encode a path for a URL, leaving the separators alone.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Escape text for HTML.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// The page every response is wrapped in.
///
/// Styles are inline and there are no other assets. The binary is copied
/// somewhere on its own — `%APPDATA%\anamnesis\bin` on the machine this was
/// written on — so anything served from a directory beside it would be a file
/// that is not there, and a server on loopback cannot borrow a stylesheet from
/// a CDN it may have no route to.
fn shell(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\
<html lang=\"en\"><head>\
<meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<meta name=\"referrer\" content=\"no-referrer\">\
<title>{title} · anamnesis</title>\
<style>{STYLE}</style>\
</head><body><main>{body}</main></body></html>",
        title = escape(title)
    )
}

/// Small enough to read, and it follows whichever theme the reader already has.
const STYLE: &str = "\
:root{color-scheme:light dark;--fg:#1a1a1a;--muted:#666;--bg:#fbfbfa;--line:#e3e3e0;--accent:#3a5bcc;--warn:#8a4b00}\
@media (prefers-color-scheme:dark){:root{--fg:#e8e8e6;--muted:#9a9a96;--bg:#17181a;--line:#2e2f33;--accent:#8aa6ff;--warn:#e0a458}}\
*{box-sizing:border-box}\
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.55 ui-sans-serif,system-ui,-apple-system,Segoe UI,sans-serif}\
main{max-width:52rem;margin:0 auto;padding:2rem 1.25rem 4rem}\
h1{font-size:1.5rem;margin:.3rem 0 .6rem}h2{font-size:1.15rem;margin:1.6rem 0 .4rem}h3{font-size:1rem;margin:1.3rem 0 .3rem}\
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}\
nav{font-size:.85rem;color:var(--muted);margin-bottom:.5rem}\
table{border-collapse:collapse;width:100%;margin:.5rem 0}\
th,td{text-align:left;padding:.45rem .5rem;border-bottom:1px solid var(--line);vertical-align:top}\
th{font-size:.75rem;text-transform:uppercase;letter-spacing:.04em;color:var(--muted);font-weight:600}\
td.num,th.num{text-align:right;white-space:nowrap;color:var(--muted);font-variant-numeric:tabular-nums}\
.path{font:12px ui-monospace,SFMono-Regular,Consolas,monospace;color:var(--muted);margin-top:.15rem}\
.muted{color:var(--muted)}\
.warn{color:var(--warn)}\
.meta{margin:.2rem 0 .8rem;display:flex;flex-wrap:wrap;gap:.35rem;align-items:center}\
.badge{display:inline-block;font-size:.72rem;padding:.1rem .4rem;border:1px solid var(--line);border-radius:.7rem;color:var(--muted)}\
.badge.pin{border-color:var(--accent);color:var(--accent)}\
.missing{color:var(--warn);border-bottom:1px dotted var(--warn)}\
article{margin-top:1rem}\
article img{max-width:100%}\
code{font:13px ui-monospace,SFMono-Regular,Consolas,monospace;background:rgba(127,127,127,.13);padding:.1rem .25rem;border-radius:3px}\
pre{overflow-x:auto;padding:.7rem;background:rgba(127,127,127,.1);border-radius:5px}\
pre code{background:none;padding:0}\
blockquote{margin:.8rem 0;padding-left:.8rem;border-left:3px solid var(--line);color:var(--muted)}\
";

#[cfg(test)]
mod tests {
    use super::*;

    use anamnesis_core::page::{Entity, Page, PageStatus, Tier};
    use anamnesis_core::scope::{ResolvedScope, resolve_scope};
    use anamnesis_store::Store;
    use anamnesis_wiki::Wiki;
    use axum::body::Body;
    use axum::http::Request;
    use base64::Engine;
    use jiff::Timestamp;
    use tower::ServiceExt;

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        state: AppState,
        scope: ResolvedScope,
    }

    fn harness() -> Harness {
        let repo = tempfile::tempdir().expect("repo dir");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");

        let data = tempfile::tempdir().expect("data dir");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");

        let scope = resolve_scope(repo.path()).expect("scope");
        store.upsert_project(&scope, now()).expect("project");

        Harness {
            state: AppState::new(store, wiki),
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn now() -> Timestamp {
        "2026-08-30T09:00:00Z".parse().expect("timestamp")
    }

    /// Write a page to the wiki and the index, as every real writer does.
    fn write(harness: &Harness, path: &str, title: &str, body: &str) -> Page {
        let mut frontmatter = Frontmatter::new(title, Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Semantic;
        frontmatter.status = PageStatus::Active;
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse(path).expect("path"),
            frontmatter,
            body,
        );
        harness
            .state
            .wiki
            .lock()
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
        harness
            .state
            .store
            .upsert_page(&page, now())
            .expect("index");
        page
    }

    async fn get_page(state: &AppState, uri: &str) -> (StatusCode, String) {
        let response = crate::router(state.clone(), true)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn the_index_lists_a_scope_and_its_page_count() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.");

        let (status, body) = get_page(&harness.state, PREFIX).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("default/widget"), "{body}");
        assert!(body.contains("/ui/default/widget"), "{body}");
    }

    #[tokio::test]
    async fn a_scope_lists_its_pages_newest_first() {
        let harness = harness();
        write(&harness, "notes/old.md", "Older page", "Body.");
        let newer = write(&harness, "notes/new.md", "Newer page", "Body.");
        harness
            .state
            .store
            .upsert_page(&newer, now() + jiff::Span::new().hours(1))
            .expect("index");

        let (status, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert_eq!(status, StatusCode::OK);
        let newer_at = body.find("Newer page").expect("newer listed");
        let older_at = body.find("Older page").expect("older listed");
        assert!(newer_at < older_at, "newest first: {body}");
    }

    #[tokio::test]
    async fn a_page_renders_its_markdown() {
        let harness = harness();
        write(
            &harness,
            "notes/one.md",
            "One",
            "## Heading\n\nSome *emphasis* and `code`.\n",
        );

        let (status, body) = get_page(&harness.state, "/ui/default/widget/notes/one.md").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("<h2>Heading</h2>"), "{body}");
        assert!(body.contains("<em>emphasis</em>"), "{body}");
    }

    /// The page body is whatever a model, a prompt, or an editor put there.
    #[tokio::test]
    async fn html_in_a_page_body_is_shown_rather_than_run() {
        let harness = harness();
        write(
            &harness,
            "notes/one.md",
            "One",
            "Before <script>alert(1)</script> after.\n",
        );

        let (status, body) = get_page(&harness.state, "/ui/default/widget/notes/one.md").await;

        assert_eq!(status, StatusCode::OK);
        assert!(!body.contains("<script>alert"), "{body}");
        assert!(
            body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{body}"
        );
    }

    /// A title comes from frontmatter, which is no more trustworthy.
    #[tokio::test]
    async fn a_title_is_escaped_too() {
        let harness = harness();
        write(&harness, "notes/one.md", "<img src=x onerror=1>", "Body.\n");

        let (_, body) = get_page(&harness.state, "/ui/default/widget/notes/one.md").await;

        assert!(!body.contains("<img src=x"), "{body}");
        assert!(body.contains("&lt;img src=x"), "{body}");
    }

    #[tokio::test]
    async fn a_wiki_link_to_a_page_that_exists_is_a_link() {
        let harness = harness();
        write(&harness, "notes/target.md", "Target", "Body.\n");
        write(
            &harness,
            "notes/one.md",
            "One",
            "See [[notes/target]] and [[notes/nothing]].\n",
        );

        let (_, body) = get_page(&harness.state, "/ui/default/widget/notes/one.md").await;

        assert!(
            body.contains("href=\"/ui/default/widget/notes/target.md\""),
            "{body}"
        );
        assert!(body.contains("class=\"missing\""), "{body}");
    }

    #[tokio::test]
    async fn an_unknown_scope_is_a_404_that_says_so() {
        let harness = harness();

        let (status, body) = get_page(&harness.state, "/ui/default/nothing").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("no scope named default/nothing"), "{body}");
    }

    /// The index can outlive the file: a page deleted by hand while the server
    /// was down is still a row until something rebuilds.
    #[tokio::test]
    async fn a_page_missing_from_the_wiki_says_what_to_run() {
        let harness = harness();
        let page = write(&harness, "notes/one.md", "One", "Body.\n");
        let file = harness
            .state
            .wiki
            .lock()
            .locate(&harness.scope.scope, &page.path);
        std::fs::remove_file(file).expect("remove");

        let (status, body) = get_page(&harness.state, "/ui/default/widget/notes/one.md").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("reindex"), "{body}");
    }

    /// Reading a page in a browser is not evidence that retrieval found it
    /// useful, and the decay sweep cannot tell the difference between the two
    /// once the counter has moved.
    #[tokio::test]
    async fn browsing_does_not_count_as_reading() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");

        get_page(&harness.state, "/ui/default/widget/notes/one.md").await;

        let rows = harness
            .state
            .store
            .sweep_rows(harness.scope.project_id)
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].facts.access_count, 0);
        assert!(rows[0].facts.last_accessed_at.is_none());
    }

    #[tokio::test]
    async fn a_path_that_climbs_out_of_the_wiki_is_refused() {
        let harness = harness();

        let (status, _) = get_page(&harness.state, "/ui/default/widget/../../etc/passwd.md").await;

        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
            "{status}"
        );
    }

    #[tokio::test]
    async fn the_browser_is_absent_when_it_is_turned_off() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");

        let response = crate::router(harness.state.clone(), false)
            .oneshot(
                Request::builder()
                    .uri(PREFIX)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Without the challenge a browser has no way to send anything: it does
    /// not know the scheme, and there is no form to type a token into.
    #[tokio::test]
    async fn a_guarded_server_asks_the_browser_for_the_token() {
        let harness = harness();
        let guarded = harness
            .state
            .clone()
            .with_auth(crate::Auth::parse(Some("s3cret"), None).expect("tokens"));
        write(&harness, "notes/one.md", "One", "Body.\n");

        let response = crate::router(guarded.clone(), true)
            .oneshot(
                Request::builder()
                    .uri(PREFIX)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            response
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("Basic ")),
            "{:?}",
            response.headers()
        );
        assert!(is_html(&response), "{:?}", response.headers());
    }

    /// Every page this module answers with is markup, including the ones that
    /// are answering with a refusal — served as `text/plain`, a reader sees the
    /// tags instead of the sentence.
    #[tokio::test]
    async fn even_the_pages_that_say_no_are_html() {
        let harness = harness();

        let response = crate::router(harness.state.clone(), true)
            .oneshot(
                Request::builder()
                    .uri("/ui/default/nothing")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(is_html(&response), "{:?}", response.headers());
    }

    fn is_html(response: &axum::response::Response) -> bool {
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"))
    }

    #[tokio::test]
    async fn the_token_typed_into_that_prompt_gets_in() {
        let harness = harness();
        let guarded = harness
            .state
            .clone()
            .with_auth(crate::Auth::parse(Some("s3cret"), None).expect("tokens"));
        write(&harness, "notes/one.md", "One", "Body.\n");

        let credential = base64::engine::general_purpose::STANDARD.encode("anyone:s3cret");
        let response = crate::router(guarded, true)
            .oneshot(
                Request::builder()
                    .uri("/ui/default/widget/notes/one.md")
                    .header(header::AUTHORIZATION, format!("Basic {credential}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");

        assert_eq!(response.status(), StatusCode::OK);
    }

    // ---------------------------------------------------------------
    // Rendering, as pure functions.
    // ---------------------------------------------------------------

    #[test]
    fn a_javascript_destination_is_defused_and_the_text_survives() {
        let html = to_html("[click](javascript:alert(1))\n", &HashMap::new());

        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains("click"), "{html}");
    }

    #[test]
    fn ordinary_destinations_are_left_alone() {
        for destination in ["https://example.org/x", "mailto:a@b.c", "../other.md#top"] {
            let html = to_html(&format!("[x]({destination})\n"), &HashMap::new());
            assert!(html.contains(destination), "{destination}: {html}");
        }
    }

    #[test]
    fn a_wiki_link_inside_a_fence_is_left_as_text() {
        let mut links = HashMap::new();
        links.insert("target".to_owned(), Some("/ui/w/p/target.md".to_owned()));

        let html = to_html("```\n[[target]]\n```\n", &links);

        assert!(!html.contains("<a href"), "{html}");
        assert!(html.contains("[[target]]"), "{html}");
    }

    #[test]
    fn text_around_a_wiki_link_is_kept() {
        let mut links = HashMap::new();
        links.insert("target".to_owned(), Some("/ui/w/p/target.md".to_owned()));

        let html = to_html("before [[target]] after\n", &links);

        assert!(html.contains("before "), "{html}");
        assert!(html.contains(" after"), "{html}");
        assert!(html.contains("href=\"/ui/w/p/target.md\""), "{html}");
    }

    #[test]
    fn empty_brackets_are_not_a_link() {
        let html = to_html("an empty [[]] pair\n", &HashMap::new());

        assert!(!html.contains("<a href"), "{html}");
        assert!(html.contains("[[]]"), "{html}");
    }

    #[test]
    fn a_path_with_a_space_survives_the_round_trip() {
        assert_eq!(encode_path("notes/a b.md"), "notes/a%20b.md");
        assert_eq!(encode_path("notes/one.md"), "notes/one.md");
    }

    #[test]
    fn entities_shown_beside_a_page_are_escaped() {
        let mut frontmatter =
            Frontmatter::new("t", vec![Entity::parse("sqlite").expect("entity")]).expect("fm");
        frontmatter.tier = Tier::Semantic;

        let html = notes(&frontmatter, None, "/ui/w/p");

        assert!(html.contains("sqlite"), "{html}");
    }
}
