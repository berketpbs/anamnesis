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

use std::collections::{HashMap, HashSet};

use anamnesis_core::page::{Frontmatter, PagePath};
use anamnesis_core::scope::Scope;
use anamnesis_store::ProjectRow;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Router, middleware};
use jiff::Timestamp;
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html::push_html};
use serde::Deserialize;

use crate::AppState;

/// Where the browser lives. One prefix, so the API keeps every path it had and
/// a proxy in front of the server can route the two apart.
pub const PREFIX: &str = "/ui";

/// The realm a browser shows in its credential prompt.
const REALM: &str = "anamnesis";

/// How many hits one search shows.
///
/// The same default `memory_query` and `anamnesis search` use. A reader who
/// has to page through results is being asked to do the ranking's job, and
/// if the answer is not in twenty the fix is in the ranking, which
/// `anamnesis eval` measures.
const SEARCH_LIMIT: usize = 20;

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

/// Every scope this server holds memory for, and whether it is still filling.
///
/// The counts are here because the question people actually arrive with is not
/// "what does memory hold" but "is memory still recording" — this repository
/// lost four days to a server that was not running, and nothing said so. A
/// scope whose last observation is a week old on a machine somebody worked in
/// yesterday is that failure, visible.
async fn index(State(state): State<AppState>) -> Result<Html<String>, UiError> {
    let projects = state.store.projects()?;
    let now = Timestamp::now();

    let mut rows = String::new();
    for project in &projects {
        let pages = state.store.page_count(project.project_id)?;
        let sessions = state.store.session_count(project.project_id)?;
        let recorded = state.store.last_observation_at(project.project_id)?;
        rows.push_str(&format!(
            "<tr><td><a href=\"{href}\">{workspace}/{project_name}</a></td>\
             <td class=\"num\">{pages}</td>\
             <td class=\"num\">{sessions}</td>\
             <td class=\"num\">{recorded}</td></tr>",
            href = escape(&scope_href(&project.scope)),
            workspace = escape(project.scope.workspace.as_str()),
            project_name = escape(project.scope.project.as_str()),
            recorded = match recorded {
                Some(at) => ago(now, at),
                // Not a fault on its own: a scope written to by hand, or one
                // whose pages came from `bootstrap`, has pages and no capture.
                None => "never".to_owned(),
            },
        ));
    }

    let body = if projects.is_empty() {
        // Empty is the normal state of a server nobody has run hooks against
        // yet, and saying so beats an empty table that reads like a fault.
        format!(
            "<h1>anamnesis</h1>{}<p class=\"muted\">No project has been registered here yet. \
             A scope appears once a session is captured for it, or once \
             <code>anamnesis init</code> runs inside a repository.</p>",
            server_facts(&state)
        )
    } else {
        format!(
            "<h1>anamnesis</h1>{facts}\
             <table><thead><tr><th>Scope</th><th class=\"num\">Pages</th>\
             <th class=\"num\">Sessions</th><th class=\"num\">Last recorded</th></tr></thead>\
             <tbody>{rows}</tbody></table>",
            facts = server_facts(&state)
        )
    };

    Ok(Html(shell("anamnesis", &body)))
}

/// What this server is doing, in the three settings that decide what memory
/// ends up being.
///
/// The same three lines `serve` prints when it starts — which is a terminal
/// nobody has open a week later, on a machine that may not be the one holding
/// the browser. No secret appears here: the token count says whether a door is
/// locked, never what opens it.
fn server_facts(state: &AppState) -> String {
    let auth = if state.auth.is_open() {
        "open — no token required".to_owned()
    } else {
        format!("token required ({} accepted)", state.auth.len())
    };
    let consolidation = match &state.llm {
        Some(settings) => format!(
            "{} ({})",
            settings.provider.model(),
            settings.provider.name()
        ),
        None => "counted — no model configured".to_owned(),
    };
    let embedding = match &state.embedder {
        Some(embedder) => embedder.model().to_owned(),
        None => "off — set ANAMNESIS_EMBED_ENABLED=1".to_owned(),
    };

    format!(
        "<dl class=\"facts\">\
         <dt>Auth</dt><dd>{}</dd>\
         <dt>Consolidation</dt><dd>{}</dd>\
         <dt>Embedding</dt><dd>{}</dd></dl>",
        escape(&auth),
        escape(&consolidation),
        escape(&embedding),
    )
}

/// How long ago something happened, in the coarsest unit that still answers
/// the question.
///
/// A reader looking at this is asking "is capture still working", and no
/// version of that question is answered better by a timestamp than by
/// "3d ago". A stamp from the future is reported as just now rather than as a
/// negative age: the clock is wrong somewhere, which is not what this column
/// is for.
fn ago(now: Timestamp, then: Timestamp) -> String {
    let seconds = (now.as_second() - then.as_second()).max(0);
    match seconds {
        0..90 => "just now".to_owned(),
        90..5400 => format!("{}m ago", seconds / 60),
        5400..172_800 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// What the reader typed into the search box, if anything.
#[derive(Debug, Deserialize)]
struct Search {
    /// The query. Absent is a plain listing; so is an empty one.
    q: Option<String>,
}

/// One scope: its pages, or the answers to a question about them.
///
/// Search lives on the listing rather than at a route of its own because it is
/// the same page about the same subject — and because a wiki's index is where
/// somebody is standing when they realise they do not know the name of the
/// thing they are looking for.
async fn scope(
    State(state): State<AppState>,
    Path((workspace, project)): Path<(String, String)>,
    Query(search): Query<Search>,
) -> Result<Html<String>, UiError> {
    let found = find_scope(&state, &workspace, &project)?;
    let query = search.q.unwrap_or_default();
    let asked = query.trim();

    let heading = format!(
        "<h1>{}/{}</h1>",
        escape(found.scope.workspace.as_str()),
        escape(found.scope.project.as_str())
    );
    let form = search_form(&scope_href(&found.scope), asked);
    let body = if asked.is_empty() {
        listing(&state, &found)?
    } else {
        results(&state, &found, asked)?
    };

    let page = format!("{}{heading}{form}{body}", crumbs(&[("anamnesis", PREFIX)]));
    Ok(Html(shell(&format!("{workspace}/{project}"), &page)))
}

/// The box the question goes in.
fn search_form(base: &str, asked: &str) -> String {
    let clear = if asked.is_empty() {
        String::new()
    } else {
        format!("<a class=\"muted\" href=\"{}\">clear</a>", escape(base))
    };
    format!(
        "<form class=\"search\" method=\"get\" action=\"{action}\">\
         <input type=\"search\" name=\"q\" value=\"{value}\" \
         placeholder=\"Ask this memory something\" aria-label=\"Search\">\
         <button type=\"submit\">Search</button>{clear}</form>",
        action = escape(base),
        value = escape(asked),
    )
}

/// Everything the index holds for one scope.
fn listing(state: &AppState, found: &ProjectRow) -> Result<String, UiError> {
    let mut pages = state.store.sweep_rows(found.project_id)?;
    let notice = drift_notice(state, found, &pages)?;
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

    if pages.is_empty() {
        return Ok(format!(
            "{notice}<p class=\"muted\">This scope has no pages in the index. \
             If the wiki has files in it, <code>anamnesis reindex</code> puts them back.</p>"
        ));
    }

    Ok(format!(
        "{notice}\
         <table><thead><tr><th>Page</th><th></th>\
         <th class=\"num\">Written</th><th class=\"num\">Reads</th></tr></thead>\
         <tbody>{rows}</tbody></table>\
         <p class=\"muted\">{count} pages. Reads are what the decay sweep counts; \
         opening one here is not one of them.</p>{proposals}",
        count = pages.len(),
        proposals = proposals(state, found)?,
    ))
}

/// What auto-improve has noticed and is waiting on a person about.
///
/// Shown and not offered: every one of these changes somebody's memory, and
/// this surface does not write. Promoting a page is a retention decision — the
/// durable tiers are the ones the decay sweep cannot reach — which is why
/// `require_approval` defaults to true, and approval means a person running
/// the command rather than a button on a page anyone who can reach the port
/// could press. The command is printed with the id already in it, because the
/// work this page can do is get somebody to the point of deciding.
///
/// Open ones only, and nothing at all when there are none. A section that is
/// always there is one nobody reads on the day it says something.
fn proposals(state: &AppState, found: &ProjectRow) -> Result<String, UiError> {
    let open = state.store.proposals(found.project_id, true)?;
    if open.is_empty() {
        return Ok(String::new());
    }

    let base = scope_href(&found.scope);
    let mut items = String::new();
    for proposal in &open {
        let id = proposal.id.to_string();
        let short = &id[..id.len().min(8)];
        // A proposal to write a missing page has nothing to link to, which is
        // the whole of what it is reporting.
        let subject = match (proposal.page_id, PagePath::parse(&proposal.subject)) {
            (Some(_), Ok(path)) => format!(
                "<a href=\"{href}\">{path}</a>",
                href = escape(&format!("{base}/{}", encode_path(path.as_str()))),
                path = escape(path.as_str()),
            ),
            _ => format!("<code>{}</code>", escape(&proposal.subject)),
        };

        items.push_str(&format!(
            "<li><strong>{action}</strong> {subject}\
             <p class=\"muted\">{rationale}</p>\
             <p class=\"path\">anamnesis improve --apply {short}</p></li>",
            action = escape(proposal.kind.action()),
            rationale = escape(&proposal.rationale),
            short = escape(short),
        ));
    }

    Ok(format!(
        "<h2>Proposals</h2>\
         <ul class=\"proposals\">{items}</ul>\
         <p class=\"muted\">{count} waiting. Nothing here changes anything: a proposal is \
         carried out by a person running the command it prints, or refused with \
         <code>anamnesis improve --dismiss</code>.</p>",
        count = open.len()
    ))
}

/// What the wiki and the index disagree about, said out loud.
///
/// The two can drift apart in both directions and neither is visible from
/// anywhere else. A page written into the wiki while the server was down is
/// not in the index, so search cannot find it however plainly it sits in the
/// editor; a row whose file was deleted the same way answers with a page that
/// is gone. `reindex` repairs both, and the watcher prevents both while the
/// server is up — but nothing said which had happened, which is why "my
/// search cannot find a page I am looking at" had no answer.
///
/// A missing scope directory is reported as itself rather than as every page
/// having been deleted. `Wiki::pages` returns the same empty list for "no
/// pages" and "no directory", and the second is a wrong data directory far
/// more often than it is a wiki somebody emptied — the same distinction
/// `reindex` refuses to delete rows over.
fn drift_notice(
    state: &AppState,
    found: &ProjectRow,
    indexed: &[anamnesis_store::SweepRow],
) -> Result<String, UiError> {
    let (on_disk, has_directory) = {
        let wiki = state.wiki.lock();
        (
            wiki.pages(&found.scope)?,
            wiki.scope_root(&found.scope).is_dir(),
        )
    };

    if !has_directory {
        if indexed.is_empty() {
            return Ok(String::new());
        }
        return Ok(format!(
            "<p class=\"warn\">The index holds {count} pages for this scope and the wiki has \
             no directory for it. That is a data directory pointing somewhere unexpected far \
             more often than it is {count} deleted pages, which is why <code>reindex</code> \
             forgets nothing while the directory is missing.</p>",
            count = indexed.len()
        ));
    }

    let rows: Vec<&str> = indexed.iter().map(|row| row.path.as_str()).collect();
    let disk: Vec<&str> = on_disk.iter().map(|path| path.as_str()).collect();
    let (unindexed, orphaned) = drift(&disk, &rows);

    let mut notice = String::new();
    if !unindexed.is_empty() {
        notice.push_str(&format!(
            "<p class=\"warn\">{count} in the wiki that the index has never seen — search \
             cannot find {them} until <code>anamnesis reindex</code> runs. {names}</p>",
            count = plural(unindexed.len(), "page", "pages"),
            them = if unindexed.len() == 1 { "it" } else { "them" },
            names = names(&unindexed),
        ));
    }
    if !orphaned.is_empty() {
        notice.push_str(&format!(
            "<p class=\"warn\">{count} in the index with no file in the wiki — opening \
             {them} says so, and <code>anamnesis reindex</code> forgets {them}. {names}</p>",
            count = plural(orphaned.len(), "page", "pages"),
            them = if orphaned.len() == 1 { "it" } else { "them" },
            names = names(&orphaned),
        ));
    }
    Ok(notice)
}

/// What each side has that the other does not.
///
/// The two directions are not symmetric — one is a page search cannot find,
/// the other is a row that answers with a file that is gone — so they are
/// returned apart and named at the call site.
fn drift<'a>(on_disk: &[&'a str], indexed: &[&'a str]) -> (Vec<&'a str>, Vec<&'a str>) {
    let disk: HashSet<&str> = on_disk.iter().copied().collect();
    let rows: HashSet<&str> = indexed.iter().copied().collect();

    let mut unindexed: Vec<&str> = disk.difference(&rows).copied().collect();
    let mut orphaned: Vec<&str> = rows.difference(&disk).copied().collect();
    // Sorted because a set has no order and a notice that reshuffles itself on
    // every refresh reads as new trouble each time.
    unindexed.sort_unstable();
    orphaned.sort_unstable();
    (unindexed, orphaned)
}

/// The first few names, which is what somebody needs to recognise the problem.
///
/// Not all of them: a wiki that was never indexed would put its whole contents
/// into a warning, and the count above already says how big it is.
fn names(paths: &[&str]) -> String {
    const SHOWN: usize = 8;
    let listed: Vec<String> = paths
        .iter()
        .take(SHOWN)
        .map(|path| format!("<code>{}</code>", escape(path)))
        .collect();
    let rest = paths.len().saturating_sub(SHOWN);
    if rest == 0 {
        listed.join(" ")
    } else {
        format!("{} and {rest} more", listed.join(" "))
    }
}

/// `1 page`, `2 pages`.
fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {many}")
    }
}

/// What the four streams make of a question.
///
/// The same call `memory_query` and `anamnesis search` make — the workspace's
/// shared scope included, and the opt-in embedder with it — so what a person
/// sees here is what an agent would have been handed. A browser that ranked
/// pages its own way would be a second retrieval that nothing measures.
///
/// This *does* record an access for every page it returns, which the page view
/// deliberately does not. They are different acts: a search hands somebody a
/// page it chose, and that is the evidence the decay sweep reads; opening a
/// page you already knew the name of is not.
fn results(state: &AppState, found: &ProjectRow, query: &str) -> Result<String, UiError> {
    // A broken or slow embedder costs the search its fourth stream, not the
    // search — the same rule an agent's query follows.
    let vector = state.embedder.as_ref().and_then(|embedder| {
        match embedder.embed(query) {
            Ok(vector) => Some((embedder.model().to_owned(), vector)),
            Err(error) => {
                tracing::warn!(%error, "query embedding failed; searching without the vector stream");
                None
            }
        }
    });

    let global = state.wiki.lock().global_scope(&found.scope.workspace);
    let hits = state.store.query_pages_across(
        found.project_id,
        &[global.project_id],
        query,
        SEARCH_LIMIT,
        Timestamp::now(),
        vector
            .as_ref()
            .map(|(model, vector)| (model.as_str(), vector.as_slice())),
    )?;

    if hits.is_empty() {
        return Ok(format!(
            "<p class=\"muted\">Nothing matched {}. Only what the index holds is \
             searchable — a page added to the wiki while the server was down reaches \
             it through <code>anamnesis reindex</code>.</p>",
            escape(&format!("{query:?}"))
        ));
    }

    let own = scope_href(&found.scope);
    let shared = scope_href(&global.scope);
    let mut rows = String::new();
    for hit in &hits {
        // Which scope a hit came from is not cosmetic: a policy that applies to
        // every project and a note about this one are different kinds of answer,
        // and the path alone does not say which is which.
        let from_shared =
            hit.project_id == global.project_id && global.project_id != found.project_id;
        let base = if from_shared { &shared } else { &own };
        let mark = if from_shared {
            format!(
                "<span class=\"badge pin\">{}</span>",
                escape(anamnesis_core::scope::GLOBAL_PROJECT)
            )
        } else {
            String::new()
        };

        rows.push_str(&format!(
            "<li><a href=\"{href}\">{title}</a> {badges}{mark}\
             <div class=\"path\">{path} · score {score:.4}</div>\
             <p class=\"muted\">{snippet}</p></li>",
            href = escape(&format!("{base}/{}", encode_path(hit.path.as_str()))),
            title = escape(&hit.title),
            badges = badges(
                hit.tier.as_str(),
                hit.status.as_str(),
                hit.pinned,
                hit.canonical
            ),
            path = escape(hit.path.as_str()),
            score = hit.score,
            snippet = escape(&hit.snippet.replace('\n', " ")),
        ));
    }

    Ok(format!(
        "<ol class=\"hits\">{rows}</ol>\
         <p class=\"muted\">{count} of at most {SEARCH_LIMIT}, ranked by the same four \
         fused streams an agent's query uses. Being handed a page counts as reading it, \
         here as in <code>anamnesis search</code>.</p>",
        count = hits.len()
    ))
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
    let retention = retention(&state, &found, &page_path)?;

    let base = scope_href(&found.scope);
    let body = format!(
        "{crumbs}<h1>{title}</h1>\
         <div class=\"meta\">{badges}<span class=\"path\">{path}</span></div>\
         {notes}{retention}\
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

/// What retention has in store for this page.
///
/// The exemptions are the half of the decay sweep that needs no configuration
/// — pinned, durable, canonical, and known-wrong put a page out of reach
/// whatever the thresholds are — so they can be stated here plainly. The score
/// cannot: it is computed from the `[decay]` table in the project's marker,
/// which lives in a working copy this server may not be able to see. Printing
/// a number from default settings would be a claim about what `anamnesis
/// sweep` will do, made by something that has not read what it reads.
///
/// The contradiction is reported rather than resolved, exactly as the sweep
/// reports it: a page that is both exempt and past its own `expires_at` is two
/// instructions from the same author, and the sweep obeys the exemption.
fn retention(state: &AppState, found: &ProjectRow, path: &PagePath) -> Result<String, UiError> {
    let Some(row) = state.store.sweep_row(found.project_id, path)? else {
        // Readable here, unfindable by search: the page is on disk and has no
        // index row. The scope's listing names every page in this state; this
        // is the one somebody is looking at.
        return Ok(
            "<p class=\"warn\">This page is not in the index — search cannot find it \
             until <code>anamnesis reindex</code> runs.</p>"
                .to_owned(),
        );
    };

    let now = Timestamp::now();
    let facts = &row.facts;
    let expired = facts.has_expired(now);

    let mut out = String::new();
    match facts.exemption() {
        Some(exemption) => {
            out.push_str(&format!(
                "<p class=\"muted\">The decay sweep does not reach this page: {}.</p>",
                escape(exemption.as_str())
            ));
            if expired {
                out.push_str(&format!(
                    "<p class=\"warn\">It also carries an expiry that passed on {}. \
                     Two instructions from the same author: the sweep keeps the page and \
                     reports the contradiction rather than resolving it.</p>",
                    facts
                        .expires_at
                        .map(|at| at.strftime("%Y-%m-%d").to_string())
                        .unwrap_or_default()
                ));
            }
        }
        None => {
            let read = match (facts.access_count, facts.last_accessed_at) {
                (0, _) | (_, None) => "never read".to_owned(),
                (count, Some(at)) => format!(
                    "read {}, last {}",
                    plural(count as usize, "time", "times"),
                    ago(now, at)
                ),
            };
            out.push_str(&format!(
                "<p class=\"muted\">The decay sweep can reach this page: {tier}, written {age}, \
                 {read}. Whether it goes depends on this project's <code>[decay]</code> settings, \
                 which <code>anamnesis sweep</code> reads — it reports by default and deletes \
                 only with <code>--apply</code>.</p>",
                tier = escape(facts.tier.as_str()),
                age = ago(now, facts.written_at),
            ));
            if expired {
                out.push_str(
                    "<p class=\"warn\">Its own expiry has passed, so the next sweep forgets it \
                     whatever its score.</p>",
                );
            }
        }
    }
    Ok(out)
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
form.search{display:flex;gap:.4rem;align-items:center;margin:.6rem 0 1rem}\
form.search input{flex:1;padding:.4rem .55rem;font:inherit;color:var(--fg);background:transparent;border:1px solid var(--line);border-radius:5px}\
form.search button{padding:.4rem .8rem;font:inherit;color:var(--bg);background:var(--accent);border:0;border-radius:5px;cursor:pointer}\
ol.hits{padding-left:1.4rem}\
ol.hits li{margin:.7rem 0}\
ol.hits p{margin:.15rem 0}\
dl.facts{display:grid;grid-template-columns:auto 1fr;gap:.15rem .8rem;margin:.4rem 0 1.2rem;font-size:.87rem}\
dl.facts dt{color:var(--muted)}\
dl.facts dd{margin:0}\
ul.proposals{list-style:none;padding:0}\
ul.proposals li{margin:.7rem 0;padding-left:.7rem;border-left:2px solid var(--line)}\
ul.proposals p{margin:.15rem 0}\
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

    use anamnesis_core::ids::PageId;
    use anamnesis_core::improve::{Proposal, ProposalKind};
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
        let scope = harness.scope.clone();
        write_in(harness, &scope, path, title, body)
    }

    /// The same, into any scope this workspace has — the shared one included.
    fn write_in(
        harness: &Harness,
        scope: &ResolvedScope,
        path: &str,
        title: &str,
        body: &str,
    ) -> Page {
        let mut frontmatter = Frontmatter::new(title, Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Semantic;
        frontmatter.status = PageStatus::Active;
        let page = Page::new(
            scope.project_id,
            PagePath::parse(path).expect("path"),
            frontmatter,
            body,
        );
        harness
            .state
            .wiki
            .lock()
            .write_page(&scope.scope, &page, "write")
            .expect("write");
        harness
            .state
            .store
            .upsert_page(&page, now())
            .expect("index");
        page
    }

    /// Put one real lifecycle event through the capture path.
    fn capture(harness: &Harness, event: &str) {
        let payload = serde_json::json!({
            "session_id": "session-ui",
            "hook_event_name": event,
            "cwd": harness.scope.root.to_string_lossy(),
        });
        let hook =
            anamnesis_hooks::parse(&anamnesis_core::session::AgentKind::ClaudeCode, &payload)
                .expect("parse");
        crate::record(&harness.state.store, None, &hook, Timestamp::now(), None).expect("record");
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

    // ---------------------------------------------------------------
    // What retention has in store for a page.
    // ---------------------------------------------------------------

    /// The durable tiers are the ones the sweep cannot reach, which is what
    /// makes promoting a page a retention decision.
    #[tokio::test]
    async fn a_durable_page_says_the_sweep_cannot_reach_it() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");

        let (_, body) = get_page(&harness.state, "/ui/default/widget/notes/one.md").await;

        assert!(body.contains("does not reach this page: durable"), "{body}");
    }

    /// And the score is not claimed: it comes from a marker this server may
    /// not be able to see.
    #[tokio::test]
    async fn a_reachable_page_says_what_it_is_judged_on_and_not_the_score() {
        let harness = harness();
        let mut frontmatter = Frontmatter::new("Ordinary", Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Episodic;
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse("notes/two.md").expect("path"),
            frontmatter,
            "Body.",
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

        let (_, body) = get_page(&harness.state, "/ui/default/widget/notes/two.md").await;

        assert!(body.contains("can reach this page: episodic"), "{body}");
        assert!(body.contains("never read"), "{body}");
        assert!(body.contains("anamnesis sweep"), "{body}");
    }

    /// Pinned and past its own deadline is two instructions from one author.
    /// The sweep obeys the pin and says so; so does this.
    #[tokio::test]
    async fn a_pinned_page_past_its_own_expiry_shows_the_contradiction() {
        let harness = harness();
        let mut frontmatter = Frontmatter::new("Kept", Vec::new()).expect("frontmatter");
        frontmatter.tier = Tier::Episodic;
        frontmatter.pinned = true;
        frontmatter.expires_at = Some(now() - jiff::Span::new().hours(48));
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse("notes/kept.md").expect("path"),
            frontmatter,
            "Body.",
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

        let (_, body) = get_page(&harness.state, "/ui/default/widget/notes/kept.md").await;

        assert!(body.contains("does not reach this page: pinned"), "{body}");
        assert!(body.contains("expiry that passed"), "{body}");
    }

    /// Readable here, unfindable by search — the page somebody is looking at
    /// while wondering why nothing returns it.
    #[tokio::test]
    async fn a_page_with_no_index_row_says_so_on_the_page_itself() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");
        let by_hand = harness.state.wiki.lock().locate(
            &harness.scope.scope,
            &PagePath::parse("notes/byhand.md").expect("path"),
        );
        std::fs::write(&by_hand, "---\ntitle: By hand\n---\n\nBody.\n").expect("write");

        let (status, body) = get_page(&harness.state, "/ui/default/widget/notes/byhand.md").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("not in the index"), "{body}");
        assert!(body.contains("reindex"), "{body}");
    }

    // ---------------------------------------------------------------
    // Proposals: shown, never offered.
    // ---------------------------------------------------------------

    /// Filed the way a pass files them, so the browser reads what the pass
    /// wrote rather than a shape invented for it.
    fn propose(harness: &Harness, kind: ProposalKind, subject: &str, page_id: Option<PageId>) {
        harness
            .state
            .store
            .record_proposals(
                harness.scope.project_id,
                &[Proposal {
                    kind,
                    subject: subject.to_owned(),
                    page_id,
                    rationale: "retrieved 4 times, 21 days old".to_owned(),
                }],
                now(),
            )
            .expect("filed");
    }

    #[tokio::test]
    async fn an_open_proposal_is_shown_with_the_command_that_carries_it_out() {
        let harness = harness();
        let page = write(&harness, "notes/one.md", "One", "Body.\n");
        propose(
            &harness,
            ProposalKind::PromoteTier,
            page.path.as_str(),
            Some(page.id),
        );

        let (status, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("promote to the semantic tier"), "{body}");
        assert!(body.contains("anamnesis improve --apply"), "{body}");
        assert!(
            body.contains("href=\"/ui/default/widget/notes/one.md\""),
            "{body}"
        );
    }

    /// A section that is always there is one nobody reads on the day it says
    /// something.
    #[tokio::test]
    async fn a_scope_with_nothing_to_propose_says_nothing() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");

        let (_, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert!(!body.contains("Proposals"), "{body}");
    }

    /// The missing page is the whole of what that proposal reports, so there
    /// is nothing to link to.
    #[tokio::test]
    async fn a_proposal_to_write_a_missing_page_links_nowhere() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");
        propose(
            &harness,
            ProposalKind::WriteMissingPage,
            "notes/absent.md",
            None,
        );

        let (_, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert!(body.contains("write the page"), "{body}");
        assert!(
            !body.contains("href=\"/ui/default/widget/notes/absent.md\""),
            "{body}"
        );
    }

    /// A subject can be any `[[target]]` a page body wrote, which means it is
    /// text a model or a prompt chose.
    #[tokio::test]
    async fn a_proposals_subject_is_escaped() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");
        propose(
            &harness,
            ProposalKind::WriteMissingPage,
            "<script>alert(1)</script>",
            None,
        );

        let (_, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert!(!body.contains("<script>alert"), "{body}");
        assert!(body.contains("&lt;script&gt;"), "{body}");
    }

    // ---------------------------------------------------------------
    // What the wiki and the index disagree about.
    // ---------------------------------------------------------------

    /// The page is right there in the editor and search will not return it.
    /// Until now nothing said why.
    #[tokio::test]
    async fn a_page_the_index_has_never_seen_is_named() {
        let harness = harness();
        write(&harness, "notes/known.md", "Known", "Body.\n");
        // Written the way an editor writes: into the wiki, with nothing told.
        let by_hand = harness.state.wiki.lock().locate(
            &harness.scope.scope,
            &PagePath::parse("notes/byhand.md").expect("path"),
        );
        std::fs::write(&by_hand, "---\ntitle: By hand\n---\n\nBody.\n").expect("write");

        let (status, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("notes/byhand.md"), "{body}");
        assert!(body.contains("reindex"), "{body}");
    }

    #[tokio::test]
    async fn a_row_whose_file_is_gone_is_named_too() {
        let harness = harness();
        let page = write(&harness, "notes/one.md", "One", "Body.\n");
        let file = harness
            .state
            .wiki
            .lock()
            .locate(&harness.scope.scope, &page.path);
        std::fs::remove_file(file).expect("remove");

        let (_, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert!(body.contains("no file in the wiki"), "{body}");
        assert!(body.contains("notes/one.md"), "{body}");
    }

    /// A wiki and an index that agree should say nothing at all: a notice that
    /// is always there is one nobody reads when it matters.
    #[tokio::test]
    async fn a_scope_that_agrees_with_itself_says_nothing() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");

        let (_, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert!(!body.contains("class=\"warn\""), "{body}");
    }

    /// `Wiki::pages` cannot tell an empty scope from an absent one, and the
    /// second is a wrong data directory far more often than it is a wiki
    /// somebody emptied — the distinction `reindex` refuses to delete over.
    #[tokio::test]
    async fn an_absent_scope_directory_is_reported_as_itself() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");
        let root = harness.state.wiki.lock().scope_root(&harness.scope.scope);
        std::fs::remove_dir_all(root).expect("remove");

        let (_, body) = get_page(&harness.state, "/ui/default/widget").await;

        assert!(body.contains("no directory for it"), "{body}");
        assert!(!body.contains("no file in the wiki"), "{body}");
    }

    #[test]
    fn each_side_is_compared_against_the_other_and_the_order_is_stable() {
        let disk = ["b.md", "a.md", "shared.md"];
        let rows = ["shared.md", "gone.md"];

        let (unindexed, orphaned) = drift(&disk, &rows);

        assert_eq!(unindexed, ["a.md", "b.md"]);
        assert_eq!(orphaned, ["gone.md"]);
    }

    #[test]
    fn a_long_list_is_cut_and_says_how_much_it_cut() {
        let many: Vec<String> = (0..10).map(|n| format!("p{n}.md")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();

        let rendered = names(&refs);

        assert!(rendered.contains("p0.md"), "{rendered}");
        assert!(rendered.ends_with("and 2 more"), "{rendered}");
    }

    /// The question people arrive with is "is memory still recording", and
    /// the answer used to be somewhere else entirely.
    #[tokio::test]
    async fn the_index_says_when_a_scope_last_recorded_anything() {
        let harness = harness();
        write(&harness, "notes/one.md", "One", "Body.\n");

        let (_, before) = get_page(&harness.state, PREFIX).await;
        assert!(before.contains("never"), "{before}");

        capture(&harness, "SessionStart");

        let (_, after) = get_page(&harness.state, PREFIX).await;
        assert!(after.contains("just now"), "{after}");
        assert!(!after.contains(">never<"), "{after}");
    }

    /// Three settings decide what memory ends up being, and `serve` says them
    /// once, to a terminal nobody has open a week later.
    #[tokio::test]
    async fn the_index_says_what_this_server_is_doing() {
        let harness = harness();

        let (_, body) = get_page(&harness.state, PREFIX).await;

        assert!(body.contains("open — no token required"), "{body}");
        assert!(body.contains("counted — no model configured"), "{body}");
        assert!(body.contains("ANAMNESIS_EMBED_ENABLED"), "{body}");
    }

    /// A locked door is worth saying; what opens it is not.
    #[tokio::test]
    async fn the_facts_never_include_the_secret() {
        let harness = harness();
        let guarded = harness
            .state
            .clone()
            .with_auth(crate::Auth::parse(None, Some("alice=s3cret")).expect("tokens"));

        let credential = base64::engine::general_purpose::STANDARD.encode("anyone:s3cret");
        let response = crate::router(guarded, true)
            .oneshot(
                Request::builder()
                    .uri(PREFIX)
                    .header(header::AUTHORIZATION, format!("Basic {credential}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("routed");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8_lossy(&bytes);

        assert!(body.contains("token required (1 accepted)"), "{body}");
        assert!(!body.contains("s3cret"), "{body}");
    }

    #[test]
    fn an_age_is_reported_in_the_coarsest_unit_that_answers_the_question() {
        let now: Timestamp = "2026-08-30T12:00:00Z".parse().expect("now");
        let seconds = |n: i64| now - jiff::Span::new().seconds(n);

        assert_eq!(ago(now, seconds(0)), "just now");
        assert_eq!(ago(now, seconds(89)), "just now");
        assert_eq!(ago(now, seconds(600)), "10m ago");
        assert_eq!(ago(now, seconds(7200)), "2h ago");
        assert_eq!(ago(now, seconds(60 * 60 * 24 * 9)), "9d ago");
        // A machine whose clock runs fast is not what this column reports on.
        assert_eq!(ago(now, seconds(-30)), "just now");
    }

    // ---------------------------------------------------------------
    // Search, which is the listing with a question on it.
    // ---------------------------------------------------------------

    /// The same fused call `memory_query` and `anamnesis search` make. A
    /// browser that ranked pages its own way would be a second retrieval that
    /// nothing measures.
    #[tokio::test]
    async fn a_question_finds_the_page_that_answers_it() {
        let harness = harness();
        write(
            &harness,
            "notes/storage.md",
            "Why SQLite",
            "The index is rebuildable, which is why SQLite is enough.",
        );
        write(
            &harness,
            "notes/versioning.md",
            "Why the wiki is a repository",
            "Pages are versioned so a deletion stays explainable.",
        );

        let (status, body) = get_page(&harness.state, "/ui/default/widget?q=sqlite").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Why SQLite"), "{body}");
        assert!(!body.contains("Why the wiki is a repository"), "{body}");
    }

    /// The counterpart to `browsing_does_not_count_as_reading`, and the reason
    /// that one is not simply "the browser never writes": a search hands
    /// somebody a page it chose, which is exactly what the counter is for.
    #[tokio::test]
    async fn being_handed_a_page_by_a_search_counts_as_reading_it() {
        let harness = harness();
        write(
            &harness,
            "notes/storage.md",
            "Why SQLite",
            "SQLite is enough.",
        );

        get_page(&harness.state, "/ui/default/widget?q=sqlite").await;

        let rows = harness
            .state
            .store
            .sweep_rows(harness.scope.project_id)
            .expect("rows");
        assert_eq!(rows[0].facts.access_count, 1);
        assert!(rows[0].facts.last_accessed_at.is_some());
    }

    #[tokio::test]
    async fn a_search_that_matches_nothing_says_what_is_searchable() {
        let harness = harness();
        write(
            &harness,
            "notes/storage.md",
            "Why SQLite",
            "SQLite is enough.",
        );

        let (status, body) = get_page(&harness.state, "/ui/default/widget?q=kubernetes").await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("reindex"), "{body}");
    }

    #[tokio::test]
    async fn an_empty_query_is_the_listing_again() {
        let harness = harness();
        write(
            &harness,
            "notes/storage.md",
            "Why SQLite",
            "SQLite is enough.",
        );

        let (_, body) = get_page(&harness.state, "/ui/default/widget?q=%20").await;

        assert!(body.contains("Why SQLite"), "{body}");
        assert!(
            body.contains("pages. Reads are what the decay sweep counts"),
            "{body}"
        );
    }

    /// The question is echoed back into an attribute, which is the shortest
    /// path from a link somebody else wrote to a script running here.
    #[tokio::test]
    async fn a_query_is_escaped_where_it_is_echoed_back() {
        let harness = harness();

        let (_, body) = get_page(
            &harness.state,
            "/ui/default/widget?q=%22%3E%3Cscript%3Ealert(1)%3C%2Fscript%3E",
        )
        .await;

        assert!(!body.contains("<script>alert"), "{body}");
        assert!(body.contains("&quot;&gt;&lt;script&gt;"), "{body}");
    }

    /// A policy in the workspace's shared scope answers a question asked in
    /// any project under it, and the two kinds of answer have to be tellable
    /// apart — the path alone does not say which is which.
    #[tokio::test]
    async fn a_hit_from_the_shared_scope_says_so_and_links_into_it() {
        let harness = harness();
        let global = harness
            .state
            .wiki
            .lock()
            .global_scope(&harness.scope.scope.workspace);
        harness
            .state
            .store
            .upsert_project(&global, now())
            .expect("shared project");
        write_in(
            &harness,
            &global,
            "policy/commits.md",
            "How commits are written",
            "Every commit message explains why, not what.",
        );

        let (_, body) = get_page(&harness.state, "/ui/default/widget?q=commits").await;

        assert!(
            body.contains("/ui/default/_global/policy/commits.md"),
            "{body}"
        );
        assert!(body.contains("_global</span>"), "{body}");
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
