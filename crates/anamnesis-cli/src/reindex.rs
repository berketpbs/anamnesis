//! Rebuilding the SQLite index from the two things that actually hold the
//! memory: the wiki and the raw spool.
//!
//! The index is disposable by design — page identifiers are derived from
//! `(project, path)` and session identifiers from `(project, agent session
//! id)`, so rebuilding reproduces exactly the same rows rather than a
//! second copy of everything. That is what makes this operation safe to run
//! against a database that still exists, not only a missing one.
//!
//! Two sources, and the split matters:
//!
//! * **`wiki/`** holds the compiled pages, and is the source of truth for
//!   them. A page edited by hand in Obsidian is picked up here the same as
//!   one this system wrote.
//! * **`raw/`** holds the observations those pages were compiled from, which
//!   exist in no wiki.
//!
//! What a rebuild deliberately does **not** restore: pending handoffs. A
//! handoff is produced by consolidation, consumed once, and is a statement
//! about what the *next* session should know — reviving one from a
//! transcript would hand a stale note to whoever starts next, which is worse
//! than starting with none.

use anamnesis_core::observation::EventKind;
use anamnesis_core::scope::{ResolvedScope, resolve_scope};
use anamnesis_core::session::Session;
use anamnesis_store::{RawRecord, RawSpool, Store};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;
use std::collections::HashSet;
use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;

use crate::format::plural;
use crate::project::global_scope;

/// What a rebuild put back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rebuilt {
    /// Pages indexed from the wiki.
    pub pages: usize,
    /// Sessions recovered from the spool.
    pub sessions: usize,
    /// Observations recovered from the spool.
    pub observations: usize,
    /// Spool files that held no session header, and so could not be attached
    /// to a session. Their observations are counted here rather than
    /// silently dropped from the report.
    pub orphaned_files: usize,
    /// Index rows dropped because the wiki no longer holds their page.
    pub removed: usize,
    /// Pages whose file is there and could not be read, by path.
    ///
    /// Named rather than counted: the index keeps whatever it last read of
    /// them, which is the right thing to do and also the reason nothing else
    /// will ever point at the file that needs fixing.
    pub unreadable: Vec<String>,
    /// Whether stale rows were left alone because the scope's wiki directory
    /// is not there at all. Reported rather than acted on: see
    /// [`rebuild_pages`].
    pub skipped_removal: bool,
}

/// Rebuild the index for one project from its wiki and spool.
///
/// Idempotent: every identifier involved is derived rather than minted, so
/// running this twice leaves the same rows rather than duplicates.
pub fn rebuild(
    store: &Store,
    wiki: &Wiki,
    raw: &RawSpool,
    scope: &ResolvedScope,
    embedder: Option<&dyn anamnesis_core::embedding::Embed>,
    now: Timestamp,
) -> anyhow::Result<Rebuilt> {
    let mut report = Rebuilt::default();
    store.upsert_project(scope, now)?;

    // Sessions before pages. A page names the session that wrote it, and into
    // an empty index a page reached first had no session to link to — which
    // failed the whole rebuild at the first model-written page, the one case
    // this command exists for. Written in this order, a rebuilt page links to
    // its session exactly as the live page did.
    let (sessions, observations, orphaned) = rebuild_sessions(store, raw, scope)?;
    report.sessions = sessions;
    report.observations = observations;
    report.orphaned_files = orphaned;
    let pages = rebuild_pages(store, wiki, scope, embedder, now)?;
    report.pages = pages.indexed;
    report.removed = pages.removed;
    report.skipped_removal = pages.skipped_removal;
    report.unreadable = pages.unreadable;

    Ok(report)
}

/// What one pass over the wiki did to the index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Pages {
    /// Pages read from the wiki and written to the index.
    indexed: usize,
    /// Rows dropped because no file answers to their path any more.
    removed: usize,
    /// Whether removal was declined because the scope directory is missing.
    skipped_removal: bool,
    /// Pages on disk that would not parse.
    unreadable: Vec<String>,
}

/// Re-index every page in the wiki, and forget the ones it no longer holds.
///
/// Links are resolved in a second pass over the same pages. One pass cannot
/// do it: a page linking to one written later would resolve against a page
/// that does not exist in the index yet, which is the exact bug the
/// backlink fix in `set_page_links` exists to prevent — and a rebuild must
/// not reintroduce it by visiting pages in an unlucky order.
///
/// Removal runs last, and only against paths the walk actually looked for.
/// A page that failed to parse is skipped above but its file is still there,
/// so it is compared by the path the walk found rather than by whether it
/// made it into the index — otherwise one malformed page would be quietly
/// forgotten instead of merely reported.
fn rebuild_pages(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    embedder: Option<&dyn anamnesis_core::embedding::Embed>,
    now: Timestamp,
) -> anyhow::Result<Pages> {
    let paths = wiki.pages(&scope.scope)?;
    let on_disk: std::collections::HashSet<String> =
        paths.iter().map(|path| path.as_str().to_owned()).collect();
    let mut report = Pages::default();
    let mut indexed = Vec::with_capacity(paths.len());

    for path in paths {
        // A page that will not parse is reported and skipped: one malformed
        // file must not cost the rebuild every page after it.
        let parsed = match wiki.read_page(&scope.scope, &path) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::warn!(%error, %path, "skipping unreadable page");
                report.unreadable.push(path.as_str().to_owned());
                continue;
            }
        };

        let entities = parsed.frontmatter.entities.clone();
        let page = anamnesis_core::page::Page::new(
            scope.project_id,
            path.clone(),
            parsed.frontmatter,
            parsed.body.clone(),
        );
        // A page the index already holds exactly is not written again.
        // `upsert_page` moves `updated_at`, which a sweep reads as when the
        // page was last written, so rebuilding an unchanged wiki would renew
        // every page in it and push the whole memory's decay clock back to
        // today. Entities and links are rebuilt either way: a page row can be
        // current while the rows that point at it are missing, which is the
        // state this command exists to repair.
        if !store.page_is_current(&page)? {
            store.upsert_page(&page, now)?;
        }
        store.set_page_entities(scope.project_id, page.id, &entities)?;
        // A rebuild has to reproduce the index the live path builds, vectors
        // included: a wiki rebuilt without them would answer differently from
        // the same wiki written page by page, which is the difference #30 was
        // about.
        store.embed_page(&page, embedder)?;
        indexed.push((page.id, parsed.body));
    }

    for (page_id, body) in &indexed {
        store.set_page_links(
            scope.project_id,
            *page_id,
            &anamnesis_wiki::extract_links(body),
        )?;
    }
    report.indexed = indexed.len();

    // An absent directory and an emptied one are the same empty list to
    // `Wiki::pages`, and they call for opposite actions: the second means
    // someone deleted their pages, the first means this rebuild is looking in
    // the wrong place — a mistyped `--data-dir`, a scope resolved from a
    // directory nobody meant. Dropping every row on the strength of a path
    // that does not exist would be obeying the typo.
    if !wiki.scope_root(&scope.scope).exists() {
        // Worth saying only when the index holds rows this absent directory
        // would otherwise have condemned. A project that has simply never been
        // written to has nothing to warn anybody about.
        report.skipped_removal = !store.page_paths(scope.project_id)?.is_empty();
        return Ok(report);
    }

    for (page_id, path) in store.page_paths(scope.project_id)? {
        if on_disk.contains(&path) {
            continue;
        }
        // Rows only. The page is already gone from the wiki — that is why we
        // are here — and its history is in the wiki's git repository, which is
        // what makes a deletion something you can look back at.
        if store.delete_page(page_id)? {
            tracing::info!(%path, "forgetting a page the wiki no longer holds");
            report.removed += 1;
        }
    }

    Ok(report)
}

/// Recover sessions and their observations from the spool.
fn rebuild_sessions(
    store: &Store,
    raw: &RawSpool,
    scope: &ResolvedScope,
) -> anyhow::Result<(usize, usize, usize)> {
    // Counted by identity, not by file: the spool starts a new file each day,
    // so a session that ran past midnight is spread over two of them.
    let mut sessions = HashSet::new();
    let mut observations = 0;
    let mut orphaned = 0;

    // Transcripts recorded before this scope was renamed into still name the
    // project they were recorded under. They are recognised by both halves
    // at once — filed in this scope's directory *and* naming a project this
    // scope used to be — because either alone is wrong: two repositories
    // with the same name and different remotes share a directory, and a
    // project renamed away from can be started again under its old name.
    let home = raw.scope_dir(&scope.scope);
    let previous = raw.previous_projects(&scope.scope)?;

    for file in raw.files()? {
        let records = raw.read_file(&file)?;

        // The header comes first in every file this crate writes, but it is
        // found by type rather than position so a file that lost its first
        // line is diagnosed rather than misread.
        let Some(session) = records.iter().find_map(|record| match record {
            RawRecord::Session(session) => Some(session.as_ref().clone()),
            RawRecord::Observation(_) => None,
        }) else {
            tracing::warn!(file = %file.display(), "spool file has no session header; skipping");
            orphaned += 1;
            continue;
        };

        // A spool holds every project under one root, so a rebuild scoped to
        // one project has to ignore the rest.
        let renamed_from = previous.contains(&session.project_id) && file.starts_with(&home);
        if session.project_id != scope.project_id && !renamed_from {
            continue;
        }
        // Filed under the project it belongs to now, which is where `rename`
        // moved the row the live index holds. Its id stays as recorded: the
        // rename kept session ids, and every observation names its session
        // by that id.
        let mut session = session;
        session.project_id = scope.project_id;

        store.ensure_session(&reopened(&session))?;
        sessions.insert(session.id);

        // The header is written once, when the file is created, and the
        // spool is append-only — so it always says the session was open,
        // even for one that ended hours later. The transcript records the
        // ending anyway: a `session-end` observation *is* the record that
        // the session closed, and its timestamp is when.
        let mut ended_at = session.ended_at;
        for record in &records {
            if let RawRecord::Observation(observation) = record {
                store.insert_observation(observation)?;
                observations += 1;
                if observation.kind == EventKind::SessionEnd {
                    ended_at = Some(observation.at);
                }
            }
        }

        // Closed after the observations are in, so the row is complete
        // before it is marked finished.
        if let Some(ended_at) = ended_at {
            store.close_session(session.id, ended_at)?;
        }
    }

    Ok((sessions.len(), observations, orphaned))
}

/// A session as it should be inserted during a rebuild.
///
/// `ensure_session` ignores a row that already exists, so the state a
/// session ends up in is decided by [`rebuild_sessions`] closing it
/// afterwards, not by what the header happened to say.
fn reopened(session: &Session) -> Session {
    let mut session = session.clone();
    session.state = anamnesis_core::session::SessionState::Open;
    session.ended_at = None;
    session
}

/// Rebuild the index from the wiki and the raw transcripts.
pub fn cmd_reindex(data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;
    data.ensure_layout()?;

    // Opening creates the database when it is missing, which is the case
    // this command exists for.
    let store = Store::open(data.db_file())?;
    store.migrate()?;
    let wiki = Wiki::open(data.wiki())?;
    let raw = anamnesis_store::RawSpool::new(data.raw());

    println!("♻️  Rebuilding the index for {}", scope.scope);
    println!(
        "   wiki:        {}",
        data.wiki_scope(&scope.scope).display()
    );
    println!("   transcripts: {}", raw.root().display());
    println!();

    let now = Timestamp::now();
    let embedder =
        anamnesis_llm::EmbedConfig::from_vars(crate::settings::var).build(&data.models())?;
    let embed = embedder
        .as_deref()
        .map(|embedder| embedder as &dyn anamnesis_core::embedding::Embed);
    let report = rebuild(&store, &wiki, &raw, &scope, embed, now)?;

    // The shared scope is rebuilt with the project, because a rebuild that
    // left it out would drop the index rows for pages every project in the
    // workspace can see — and nothing else would ever put them back.
    let global = global_scope(&scope, &data);
    let shared = if data.wiki_global(&scope.scope.workspace).exists() {
        Some(rebuild(&store, &wiki, &raw, &global, embed, now)?)
    } else {
        None
    };

    println!("  {} page(s) indexed", report.pages);
    if let Some(shared) = &shared {
        println!(
            "  {} page(s) indexed in {}",
            shared.pages, global.scope.project
        );
    }
    println!(
        "  {} session(s), {} observation(s) recovered",
        report.sessions, report.observations
    );
    if !report.unreadable.is_empty() {
        println!(
            "  {} could not be read and kept what the index last had:",
            plural(report.unreadable.len() as i64, "page")
        );
        for path in &report.unreadable {
            println!("    {path}");
        }
    }
    if report.orphaned_files > 0 {
        println!(
            "  {} transcript file(s) had no session header and were skipped",
            report.orphaned_files
        );
    }
    if report.removed > 0 {
        println!(
            "  {} forgotten — no longer in the wiki",
            plural(report.removed as i64, "page")
        );
    }
    if report.skipped_removal {
        println!();
        println!("  ⚠ No wiki directory at that path, so nothing was forgotten.");
        println!("    An index with rows and a scope with no directory usually");
        println!("    means this ran against the wrong data dir or project.");
    }
    println!();
    println!("  Pending handoffs are not restored: a handoff says what the *next*");
    println!("  session should know, and reviving a stale one is worse than none.");

    Ok(())
}

/// What comparing one scope with a rebuild of it found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checked {
    /// Where the index differs from the rebuild, pages that could not be read
    /// left out of it.
    pub drift: anamnesis_store::Drift,
    /// Pages whose file is there and would not parse.
    ///
    /// Kept apart because the rebuild skips them and the index keeps them,
    /// which reads as "only in the index" — and the remedy for that, a
    /// reindex, leaves them exactly where they are. The file is what needs
    /// fixing.
    pub unreadable: Vec<String>,
}

impl Checked {
    /// Whether the index is exactly what a rebuild would produce.
    pub fn is_clean(&self) -> bool {
        self.drift.is_empty() && self.unreadable.is_empty()
    }
}

/// Rebuild one scope into a scratch index and compare it with `live`.
///
/// The scratch index is in memory and nothing is embedded: vectors are not
/// compared, and asking a model for every page to throw the answers away
/// would make a read-only check the slowest command there is.
pub fn check(
    live: &Store,
    wiki: &Wiki,
    raw: &RawSpool,
    scope: &ResolvedScope,
    now: Timestamp,
) -> anyhow::Result<Checked> {
    let rebuilt = Store::open_in_memory()?;
    rebuilt.migrate()?;
    let report = rebuild(&rebuilt, wiki, raw, scope, None, now)?;
    let mut drift = live.drift_from(&rebuilt, scope.project_id)?;

    // An unreadable page's rows are the index's last good reading of it, and
    // a reindex keeps them. Reported once, as the page, not again as every
    // name and link it held.
    let unreadable = report.unreadable;
    let of_unreadable = |label: &String| {
        unreadable
            .iter()
            .any(|path| label == path || label.starts_with(&format!("{path} → ")))
    };
    drift.pages.only_live.retain(|label| !of_unreadable(label));
    drift
        .entities
        .only_live
        .retain(|label| !of_unreadable(label));
    drift.links.only_live.retain(|label| !of_unreadable(label));

    Ok(Checked { drift, unreadable })
}

/// Compare the index in use with what `reindex` would build, writing nothing.
///
/// The index is only safe to lose if a rebuild reproduces it, and nothing
/// else ever tests that: a path that writes a row without its durable copy is
/// found the day the database is gone. This finds it on a day it is not.
pub fn cmd_reindex_check(data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;

    // Every open below would otherwise create what it opens, and a check that
    // leaves an empty database or wiki behind has written something after all.
    let db = data.db_file();
    if !db.exists() {
        anyhow::bail!("there is no index at {} to compare", db.display());
    }
    if !data.wiki().exists() {
        anyhow::bail!(
            "there is no wiki at {} to rebuild from",
            data.wiki().display()
        );
    }

    // Not migrated: an index a step behind this build would be reported as
    // drifting by exactly the migration, which is not what anybody asked.
    let live = Store::open(&db)?;
    let scratch = Store::open_in_memory()?;
    scratch.migrate()?;
    let (ours, theirs) = (live.schema_version()?, scratch.schema_version()?);
    if ours != theirs {
        anyhow::bail!(
            "the index is at schema {} and this build at {}; start the server from this build to migrate it, then check again",
            ours.map_or("none".to_owned(), |v| v.to_string()),
            theirs.map_or("none".to_owned(), |v| v.to_string()),
        );
    }
    drop(scratch);

    let wiki = Wiki::open(data.wiki())?;
    let raw = RawSpool::new(data.raw());

    println!("🔎 Comparing the index with a rebuild of {}", scope.scope);
    println!("   index:       {}", db.display());
    println!(
        "   wiki:        {}",
        data.wiki_scope(&scope.scope).display()
    );
    println!("   transcripts: {}", raw.root().display());

    let now = Timestamp::now();
    let mut drifted = print_drift(
        &scope.scope.to_string(),
        &check(&live, &wiki, &raw, &scope, now)?,
    );
    let global = global_scope(&scope, &data);
    if data.wiki_global(&scope.scope.workspace).exists() {
        drifted |= print_drift(
            &global.scope.to_string(),
            &check(&live, &wiki, &raw, &global, now)?,
        );
    }

    println!();
    println!("  Not compared: vectors, handoffs, access counts, session state.");
    println!("  A rebuild does not reproduce those, by design.");
    if !drifted {
        return Ok(());
    }
    println!();
    println!("  A row written in the last few seconds can show up while the server");
    println!("  is between the wiki and the index; check again before acting on it.");
    anyhow::bail!("the index differs from what a rebuild would produce")
}

/// Print one scope's comparison. Returns whether anything differed.
fn print_drift(scope: &str, checked: &Checked) -> bool {
    let drift = &checked.drift;
    println!();
    println!("  {scope}");
    if !checked.unreadable.is_empty() {
        println!(
            "    {} on disk that will not parse — the index keeps its last copy, and reindex cannot change that:",
            plural(checked.unreadable.len() as i64, "page")
        );
        for path in &checked.unreadable {
            println!("        {path}");
        }
    }
    print_divergence(
        "pages",
        &drift.pages,
        "in the index, not in the wiki — reindex drops them",
        "in the wiki, not in the index — reindex adds them",
        "differ from their page — reindex rewrites them",
    );
    print_divergence(
        "entities",
        &drift.entities,
        "filed in the index, not by any page — reindex drops them",
        "named by a page, missing from the index — reindex adds them",
        "",
    );
    print_divergence(
        "links",
        &drift.links,
        "in the index, not in any page — reindex drops them",
        "in a page, missing from the index — reindex adds them",
        "resolve differently from a rebuild — reindex re-resolves them",
    );
    print_divergence(
        "observations",
        &drift.observations,
        "in the index and in no transcript — lost if the index is, and reindex cannot bring them back",
        "in a transcript, missing from the index — reindex restores them",
        "differ from their transcript — reindex keeps the index's copy",
    );
    !checked.is_clean()
}

/// How many examples of one kind of difference are worth printing.
///
/// Enough to recognise a pattern — a day, a session, a directory — and few
/// enough that a thousand of the same thing does not bury the summary.
const EXAMPLES: usize = 5;

fn print_divergence(
    noun: &str,
    divergence: &anamnesis_store::Divergence,
    only_live: &str,
    only_rebuilt: &str,
    differing: &str,
) {
    if divergence.is_empty() {
        println!("    {noun:<13} {} compared, in step", divergence.live);
        return;
    }
    println!("    {noun:<13} {} compared", divergence.live);
    let group = |count: usize, what: &str, labels: &mut dyn Iterator<Item = String>| {
        if count == 0 {
            return;
        }
        println!("      {count} {what}");
        for label in labels.take(EXAMPLES) {
            println!("        {label}");
        }
        if count > EXAMPLES {
            println!("        … and {} more", count - EXAMPLES);
        }
    };
    group(
        divergence.only_live.len(),
        only_live,
        &mut divergence.only_live.iter().cloned(),
    );
    group(
        divergence.only_rebuilt.len(),
        only_rebuilt,
        &mut divergence.only_rebuilt.iter().cloned(),
    );
    group(
        divergence.differing.len(),
        differing,
        &mut divergence
            .differing
            .iter()
            .map(|(label, columns)| format!("{label}  ({})", columns.join(", "))),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::observation::BoundedBody;
    use anamnesis_core::page::{Frontmatter, Page, PagePath};
    use anamnesis_core::scope::resolve_scope;
    use anamnesis_core::session::AgentKind;
    use anamnesis_store::{new_observation, new_session};

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        store: Store,
        wiki: Wiki,
        raw: RawSpool,
        scope: ResolvedScope,
    }

    fn harness() -> Harness {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(
            repo.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let scope = resolve_scope(repo.path()).expect("scope");

        let data = tempfile::tempdir().expect("data");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");

        Harness {
            wiki: Wiki::open(data.path().join("wiki")).expect("wiki"),
            raw: RawSpool::new(data.path().join("raw")),
            store,
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    /// Write a page to the wiki only — as if the index had been lost.
    fn wiki_page(harness: &Harness, path: &str, title: &str, body: &str) {
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse(path).expect("path"),
            Frontmatter::new(title, Vec::new()).expect("frontmatter"),
            body,
        );
        harness
            .wiki
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
    }

    /// Spool a session and its observations, again bypassing the index.
    fn spool_session(harness: &Harness, agent_session: &str, bodies: &[&str]) -> Session {
        let session = new_session(
            anamnesis_core::ids::SessionId::derive(harness.scope.project_id, agent_session),
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
            None,
        );
        for body in bodies {
            let observation = new_observation(
                session.id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating(*body, 1024),
                now(),
            );
            harness
                .raw
                .append(&harness.scope.scope, &session, &observation)
                .expect("spool");
        }
        session
    }

    #[test]
    fn a_rebuild_recovers_pages_sessions_and_observations() {
        let harness = harness();
        wiki_page(
            &harness,
            "decisions/0001-storage.md",
            "Storage engine",
            "We chose SQLite.",
        );
        spool_session(&harness, "session-1", &["first prompt", "second prompt"]);

        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");

        assert_eq!(report.pages, 1);
        assert_eq!(report.sessions, 1);
        assert_eq!(report.observations, 2);
        assert_eq!(report.orphaned_files, 0);

        // And the rebuilt rows are actually queryable.
        let hits = harness
            .store
            .query_pages(harness.scope.project_id, "sqlite", 10, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Storage engine");
    }

    /// The case this command exists for, and the one it could not do: an
    /// index gone entirely, a wiki whose pages name the sessions that wrote
    /// them. Pages were rebuilt before sessions, so the first page naming one
    /// failed a foreign key and the rebuild stopped there. A page naming a
    /// session nothing holds any more — forgotten, or never spooled — is
    /// rebuilt unlinked rather than failing, and a second rebuild leaves both
    /// pages alone instead of rewriting them.
    #[test]
    fn a_rebuild_into_an_empty_index_links_pages_to_their_sessions() {
        let harness = harness();
        let session = spool_session(&harness, "session-1", &["what did we decide"]);

        let write = |path: &str, names: anamnesis_core::ids::SessionId| {
            let mut frontmatter = Frontmatter::new("A session page", Vec::new()).expect("fm");
            frontmatter.session = Some(names);
            let page = Page::new(
                harness.scope.project_id,
                PagePath::parse(path).expect("path"),
                frontmatter,
                "body",
            );
            harness
                .wiki
                .write_page(&harness.scope.scope, &page, "write")
                .expect("write");
        };
        write("sessions/2026-08-25-spooled.md", session.id);
        let forgotten = anamnesis_core::ids::SessionId::derive(harness.scope.project_id, "gone");
        write("sessions/2026-08-24-forgotten.md", forgotten);

        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("a rebuild into an empty index");
        assert_eq!((report.pages, report.sessions), (2, 1));

        let linked = |path: &str| -> (Option<String>, String) {
            harness
                .store
                .connection()
                .query_row(
                    "SELECT session_id, updated_at FROM pages WHERE path = ?1",
                    [path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("page row")
        };
        let (spooled, first_write) = linked("sessions/2026-08-25-spooled.md");
        assert_eq!(spooled, Some(session.id.to_string()));
        let (unlinked, _) = linked("sessions/2026-08-24-forgotten.md");
        assert_eq!(unlinked, None, "a session nothing holds is left unlinked");

        let later: Timestamp = "2026-09-01T09:00:00Z".parse().expect("later");
        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            later,
        )
        .expect("second rebuild");
        assert_eq!(linked("sessions/2026-08-25-spooled.md").1, first_write);
        assert_eq!(
            linked("sessions/2026-08-24-forgotten.md").1,
            first_write,
            "a page naming a forgotten session is current, not rewritten every time"
        );
    }

    /// Before capture wrote its header from the stored session, one that ran
    /// past midnight was filed under two dates. Both files are recovered, and
    /// the report counts the session they belong to once.
    #[test]
    fn a_session_filed_under_two_dates_is_counted_once() {
        let harness = harness();
        let mut session = spool_session(&harness, "session-1", &["before midnight"]);
        session.started_at = "2026-08-26T00:10:00Z".parse().expect("next day");
        let observation = new_observation(
            session.id,
            EventKind::UserPrompt,
            None,
            BoundedBody::truncating("after midnight", 1024),
            session.started_at,
        );
        harness
            .raw
            .append(&harness.scope.scope, &session, &observation)
            .expect("spool");
        assert_eq!(
            harness
                .raw
                .locate_all(&harness.scope.scope, session.id)
                .len(),
            2
        );

        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");

        assert_eq!((report.sessions, report.observations), (1, 2));
    }

    #[test]
    fn rebuilding_twice_leaves_the_same_rows() {
        // Identifiers are derived, so a second run must not duplicate
        // anything - this is what makes reindex safe on a live database.
        let harness = harness();
        wiki_page(&harness, "notes/a.md", "A", "body");
        spool_session(&harness, "session-1", &["one"]);

        let first = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("first");
        let second = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("second");

        assert_eq!(first, second);
        assert_eq!(
            harness
                .store
                .page_count(harness.scope.project_id)
                .expect("pages"),
            1
        );
        assert_eq!(
            harness
                .store
                .session_count(harness.scope.project_id)
                .expect("sessions"),
            1
        );
        let session = anamnesis_core::ids::SessionId::derive(harness.scope.project_id, "session-1");
        assert_eq!(
            harness.store.observations(session).expect("obs").len(),
            1,
            "observation ids are derived from the record, not minted per run"
        );
    }

    #[test]
    fn supersession_resolves_regardless_of_the_order_pages_are_visited() {
        // `a.md` replaces `z.md`, which sorts last and is therefore indexed
        // after it. The claim is recorded as authored and resolved when the
        // page it names arrives, so a rebuild cannot lose it by walking the
        // wiki in the order the filesystem happens to hand it over.
        let harness = harness();
        let mut frontmatter = Frontmatter::new("A", Vec::new()).expect("frontmatter");
        frontmatter.supersedes = Some(PagePath::parse("z.md").expect("path"));
        let replacement = Page::new(
            harness.scope.project_id,
            PagePath::parse("a.md").expect("path"),
            frontmatter,
            "The page that replaces z.",
        );
        harness
            .wiki
            .write_page(&harness.scope.scope, &replacement, "write")
            .expect("write");
        wiki_page(&harness, "z.md", "Z", "The page being replaced.");

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");

        let heads: Vec<String> = harness
            .store
            .connection()
            .prepare("SELECT path FROM pages WHERE is_latest = 1 ORDER BY path")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<String>, _>>()
            .expect("rows");
        assert_eq!(heads, vec!["a.md".to_owned()], "z.md was replaced");
    }

    #[test]
    fn links_resolve_regardless_of_the_order_pages_are_visited() {
        // `a.md` links to `z.md`, which sorts last and is therefore indexed
        // after it. A single-pass rebuild would leave that link dangling.
        let harness = harness();
        wiki_page(&harness, "a.md", "A", "See [[z.md]].");
        wiki_page(&harness, "z.md", "Z", "The target.");

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");

        let unresolved: i64 = harness
            .store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM page_links WHERE to_page_id IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(unresolved, 0, "every link should have resolved");
    }

    #[test]
    fn a_closed_session_comes_back_closed() {
        let harness = harness();
        let mut session = spool_session(&harness, "session-1", &["work"]);

        // Re-spool the header with an end time, the way a closed session's
        // transcript looks.
        session.ended_at = Some("2026-08-25T10:00:00Z".parse().unwrap());
        let path = harness.raw.locate(&harness.scope.scope, &session);
        let text = std::fs::read_to_string(&path).expect("read");
        let rewritten: Vec<String> = text
            .lines()
            .map(|line| {
                if line.contains("\"type\":\"session\"") {
                    serde_json::to_string(&RawRecord::Session(Box::new(session.clone())))
                        .expect("encode")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        std::fs::write(&path, rewritten.join("\n") + "\n").expect("write");

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");

        let loaded = harness
            .store
            .load_session(session.id)
            .expect("load")
            .expect("found");
        assert!(!loaded.is_open());
        assert_eq!(loaded.ended_at, session.ended_at);
    }

    #[test]
    fn a_spool_file_with_no_header_is_reported_not_silently_dropped() {
        let harness = harness();
        let dir = harness
            .raw
            .root()
            .join("default")
            .join("widget")
            .join("2026-08-25");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("orphan.jsonl"),
            "{\"type\":\"observation\",\"id\":\"01a035a6-be79-7c92-8718-3dafc327751d\",\
             \"session_id\":\"01a035a6-be79-7c92-8718-3dafc327751e\",\"kind\":\"user-prompt\",\
             \"tool\":null,\"at\":\"2026-08-25T09:00:00Z\",\
             \"body\":{\"text\":\"lost\",\"truncated\":false},\"sanitized\":true}\n",
        )
        .expect("write");

        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");

        assert_eq!(report.orphaned_files, 1);
        assert_eq!(report.sessions, 0);
    }

    #[test]
    fn a_session_end_in_the_transcript_closes_the_rebuilt_session() {
        // The spool header is written once, at file creation, and never
        // updated - so it always says "open". Without reading the
        // session-end observation, every rebuilt session would come back
        // open, which is what the first real run of `reindex` actually did.
        let harness = harness();
        let session = new_session(
            anamnesis_core::ids::SessionId::derive(harness.scope.project_id, "session-1"),
            harness.scope.project_id,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
            None,
        );
        assert!(session.is_open(), "the header is written while still open");

        let ended: Timestamp = "2026-08-25T10:30:00Z".parse().unwrap();
        for (kind, body, at) in [
            (EventKind::UserPrompt, "work", now()),
            (EventKind::SessionEnd, "clear", ended),
        ] {
            let observation = new_observation(
                session.id,
                kind,
                None,
                BoundedBody::truncating(body, 1024),
                at,
            );
            harness
                .raw
                .append(&harness.scope.scope, &session, &observation)
                .expect("spool");
        }

        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");

        let loaded = harness
            .store
            .load_session(session.id)
            .expect("load")
            .expect("found");
        assert!(
            !loaded.is_open(),
            "a session that ended must come back closed"
        );
        assert_eq!(loaded.ended_at, Some(ended), "and at the time it ended");
    }

    /// Rebuild once, so the index holds whatever the wiki holds.
    fn rebuilt(harness: &Harness) -> Rebuilt {
        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild")
    }

    /// The file a page lives in, for tests that reach past the wiki API.
    fn page_file(harness: &Harness, path: &str) -> std::path::PathBuf {
        harness.wiki.scope_root(&harness.scope.scope).join(path)
    }

    /// The gap this closes: search kept offering a page that was not there any
    /// more, because a rebuild only ever added.
    #[test]
    fn a_page_deleted_from_the_wiki_is_forgotten_by_the_index() {
        let harness = harness();
        wiki_page(&harness, "kept.md", "Kept", "Still here.");
        wiki_page(&harness, "gone.md", "Gone", "Deleted by hand.");
        assert_eq!(rebuilt(&harness).pages, 2);

        std::fs::remove_file(page_file(&harness, "gone.md")).expect("delete");

        let report = rebuilt(&harness);
        assert_eq!(report.pages, 1);
        assert_eq!(report.removed, 1);
        assert!(!report.skipped_removal);

        let left: Vec<String> = harness
            .store
            .page_paths(harness.scope.project_id)
            .expect("paths")
            .into_iter()
            .map(|(_, path)| path)
            .collect();
        assert_eq!(left, vec!["kept.md".to_owned()]);
    }

    /// Rebuilding an unchanged wiki must not move `updated_at`, which a sweep
    /// reads as when the page was last written. Renewing every page on every
    /// rebuild would push the whole memory's decay clock back to today.
    #[test]
    fn rebuilding_an_unchanged_page_does_not_renew_it() {
        let harness = harness();
        wiki_page(&harness, "note.md", "Note", "Body.");
        rebuilt(&harness);

        let written_at = |harness: &Harness| -> String {
            harness
                .store
                .connection()
                .query_row("SELECT updated_at FROM pages", [], |row| row.get(0))
                .expect("updated_at")
        };
        let first = written_at(&harness);

        // A later `now` that would be written if the page were touched at all.
        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            "2026-09-01T09:00:00Z".parse().expect("timestamp"),
        )
        .expect("rebuild");
        assert_eq!(
            written_at(&harness),
            first,
            "an unchanged page is not renewed"
        );

        // But an edited one is: the content a reader would find is that recent.
        wiki_page(&harness, "note.md", "Note", "Edited body.");
        rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            "2026-09-02T09:00:00Z".parse().expect("timestamp"),
        )
        .expect("rebuild");
        assert_ne!(written_at(&harness), first, "an edited page is renewed");
    }

    /// A page that will not parse is skipped, not absent. Comparing against
    /// what made it into the index rather than what the walk found would
    /// quietly forget it — and the one page anyone needs to fix is the one
    /// that would disappear.
    #[test]
    fn a_page_that_will_not_parse_is_skipped_not_forgotten() {
        let harness = harness();
        wiki_page(&harness, "broken.md", "Broken", "Fine for now.");
        assert_eq!(rebuilt(&harness).pages, 1);

        std::fs::write(page_file(&harness, "broken.md"), "no frontmatter here").expect("corrupt");

        let report = rebuilt(&harness);
        assert_eq!(report.pages, 0, "it could not be read");
        assert_eq!(report.removed, 0, "but it is still there");
        assert_eq!(
            harness
                .store
                .page_paths(harness.scope.project_id)
                .expect("paths")
                .len(),
            1
        );
    }

    /// An absent directory and an emptied one look identical to `Wiki::pages`,
    /// and call for opposite actions. Dropping every row on the strength of a
    /// path that does not exist would be obeying a mistyped `--data-dir`.
    #[test]
    fn a_missing_scope_directory_forgets_nothing_and_says_so() {
        let harness = harness();
        wiki_page(&harness, "page.md", "Page", "Body.");
        assert_eq!(rebuilt(&harness).pages, 1);

        std::fs::remove_dir_all(harness.wiki.scope_root(&harness.scope.scope))
            .expect("remove scope directory");

        let report = rebuilt(&harness);
        assert_eq!(report.removed, 0);
        assert!(report.skipped_removal);
        assert_eq!(
            harness
                .store
                .page_paths(harness.scope.project_id)
                .expect("paths")
                .len(),
            1,
            "the row survives a wiki that is merely not where we looked"
        );
    }

    /// Forgetting a page must not take the links other pages wrote to it. The
    /// target is gone; the fact that someone linked to it is not, and the link
    /// has to resolve again if the page comes back.
    #[test]
    fn links_to_a_forgotten_page_survive_as_unresolved() {
        let harness = harness();
        wiki_page(&harness, "from.md", "From", "See [[to.md]].");
        wiki_page(&harness, "to.md", "To", "The target.");
        rebuilt(&harness);

        let links = |predicate: &str| -> i64 {
            harness
                .store
                .connection()
                .query_row(
                    &format!("SELECT COUNT(*) FROM page_links WHERE {predicate}"),
                    [],
                    |row| row.get(0),
                )
                .expect("count")
        };
        assert_eq!(links("1 = 1"), 1);
        assert_eq!(links("to_page_id IS NULL"), 0);

        std::fs::remove_file(page_file(&harness, "to.md")).expect("delete");
        assert_eq!(rebuilt(&harness).removed, 1);

        assert_eq!(links("1 = 1"), 1, "the link someone wrote is still a fact");
        assert_eq!(links("to_page_id IS NULL"), 1, "it just points at nothing");

        wiki_page(&harness, "to.md", "To", "The target.");
        assert_eq!(rebuilt(&harness).removed, 0);
        assert_eq!(links("to_page_id IS NULL"), 0, "and resolves again");
    }

    #[test]
    fn an_empty_wiki_and_spool_rebuild_to_nothing() {
        let harness = harness();
        let report = rebuild(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            None,
            now(),
        )
        .expect("rebuild");
        assert_eq!(report, Rebuilt::default());
    }

    fn checked(harness: &Harness) -> Checked {
        check(
            &harness.store,
            &harness.wiki,
            &harness.raw,
            &harness.scope,
            now(),
        )
        .expect("check")
    }

    /// A transcript compacted a week after it went quiet, and then resumed:
    /// half of it compressed, half plain. It is rebuilt whole, and the index
    /// it rebuilds to checks clean against it.
    #[test]
    fn a_compacted_transcript_is_rebuilt_and_checked_like_a_plain_one() {
        let harness = harness();
        let session = spool_session(&harness, "session-1", &["first", "second"]);
        let plain = harness.raw.locate(&harness.scope.scope, &session);
        assert!(harness.raw.compact(&plain).expect("compact"));
        let resumed = new_observation(
            session.id,
            EventKind::UserPrompt,
            None,
            BoundedBody::truncating("third", 1024),
            now(),
        );
        harness
            .raw
            .append(&harness.scope.scope, &session, &resumed)
            .expect("spool");

        let report = rebuilt(&harness);

        assert_eq!(report.sessions, 1);
        assert_eq!(report.observations, 3);
        assert_eq!(report.orphaned_files, 0);
        let checked = checked(&harness);
        assert!(checked.is_clean(), "{checked:?}");
        assert_eq!(checked.drift.observations.live, 3);
    }

    /// The baseline every other check is read against: an index that is
    /// exactly what its sources rebuild to reports nothing. A check that
    /// flagged a healthy memory would be switched off by the first person to
    /// run it.
    #[test]
    fn an_index_its_sources_rebuild_to_is_clean() {
        let harness = harness();
        wiki_page(&harness, "decisions/a.md", "A", "Links to [[b]].");
        wiki_page(&harness, "b.md", "B", "Nothing.");
        spool_session(&harness, "session-1", &["first", "second"]);
        rebuilt(&harness);

        let checked = checked(&harness);

        assert!(checked.is_clean(), "{checked:?}");
        assert_eq!(checked.drift.pages.live, 2);
        assert_eq!(checked.drift.observations.live, 2);
    }

    /// What the check is for. The server writes the index first and the
    /// spool second, and a spool write that fails is logged and stepped over
    /// — which leaves an observation that exists only in the one copy this
    /// system calls disposable.
    #[test]
    fn an_observation_that_never_reached_a_transcript_is_reported() {
        let harness = harness();
        let session = spool_session(&harness, "session-1", &["spooled"]);
        rebuilt(&harness);
        harness
            .store
            .insert_observation(&new_observation(
                session.id,
                EventKind::UserPrompt,
                None,
                BoundedBody::truncating("never spooled", 1024),
                now(),
            ))
            .expect("insert");

        let checked = checked(&harness);

        assert_eq!(checked.drift.observations.only_live.len(), 1, "{checked:?}");
        assert!(!checked.is_clean());
    }

    /// Read-only means read-only: a page the index is missing is reported,
    /// and still missing afterwards. The rebuild happened somewhere else.
    #[test]
    fn a_check_writes_nothing_to_the_index_it_checks() {
        let harness = harness();
        wiki_page(&harness, "decisions/a.md", "A", "Indexed.");
        rebuilt(&harness);
        wiki_page(&harness, "decisions/late.md", "Late", "Only on disk.");
        spool_session(&harness, "session-1", &["only in the transcript"]);

        let checked = checked(&harness);

        assert_eq!(checked.drift.pages.only_rebuilt, ["decisions/late.md"]);
        assert_eq!(checked.drift.observations.only_rebuilt.len(), 1);
        assert_eq!(
            harness
                .store
                .page_count(harness.scope.project_id)
                .expect("pages"),
            1
        );
        assert_eq!(
            harness
                .store
                .session_count(harness.scope.project_id)
                .expect("sessions"),
            0
        );
    }

    /// A page that will not parse is skipped by the rebuild and kept by the
    /// index, which reads as "only in the index" — and the remedy printed
    /// for that, a reindex, changes nothing. It is reported once, as a file
    /// to fix, and not again as every name and link it held.
    #[test]
    fn an_unreadable_page_is_reported_as_the_file_and_nothing_else() {
        let harness = harness();
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse("broken.md").expect("path"),
            Frontmatter::new(
                "Broken",
                vec![anamnesis_core::page::Entity::parse("redis").expect("entity")],
            )
            .expect("frontmatter"),
            "Links to [[elsewhere]].",
        );
        harness
            .wiki
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
        rebuilt(&harness);
        std::fs::write(page_file(&harness, "broken.md"), "no frontmatter here").expect("corrupt");

        let checked = checked(&harness);

        assert_eq!(checked.unreadable, ["broken.md"]);
        assert!(checked.drift.is_empty(), "{:?}", checked.drift);
        assert!(!checked.is_clean());
    }

    /// A session recorded under `project` and filed in `filed_in`'s
    /// directory, written to the spool only — as `rename` leaves one.
    fn spool_foreign(
        harness: &Harness,
        project: anamnesis_core::ids::ProjectId,
        filed_in: &anamnesis_core::scope::Scope,
    ) -> Session {
        let session = new_session(
            anamnesis_core::ids::SessionId::derive(project, "recorded-before"),
            project,
            harness.scope.workspace_id,
            AgentKind::ClaudeCode,
            "/repo".into(),
            now(),
            None,
        );
        let observation = new_observation(
            session.id,
            EventKind::UserPrompt,
            None,
            BoundedBody::truncating("before the rename", 1024),
            now(),
        );
        harness
            .raw
            .append(filed_in, &session, &observation)
            .expect("spool");
        session
    }

    fn scope_named(name: &str) -> anamnesis_core::scope::Scope {
        anamnesis_core::scope::Scope {
            workspace: anamnesis_core::scope::WorkspaceName::parse("default").expect("workspace"),
            project: anamnesis_core::scope::ProjectName::parse(name).expect("name"),
        }
    }

    fn old_project() -> anamnesis_core::ids::ProjectId {
        anamnesis_core::ids::ProjectId::from_uuid(uuid::Uuid::from_u128(0x01d))
    }

    /// The rename case: filed here, naming the project this scope was before.
    /// It comes back under the project it belongs to now, keeping the id
    /// every one of its observations names it by.
    #[test]
    fn a_transcript_recorded_before_a_rename_is_rebuilt_under_the_new_project() {
        let harness = harness();
        harness
            .raw
            .record_previous(
                &harness.scope.scope,
                old_project(),
                &scope_named("old-name"),
            )
            .expect("note");
        let session = spool_foreign(&harness, old_project(), &harness.scope.scope);

        let report = rebuilt(&harness);

        assert_eq!((report.sessions, report.observations), (1, 1));
        let stored = harness
            .store
            .load_session(session.id)
            .expect("load")
            .expect("the session is back");
        assert_eq!(stored.project_id, harness.scope.project_id);
    }

    /// Two repositories with the same name and different remotes resolve to
    /// the same directory and different projects. The directory alone must
    /// not hand one of them the other's sessions.
    #[test]
    fn a_neighbour_sharing_the_directory_is_not_adopted() {
        let harness = harness();
        let neighbour = anamnesis_core::ids::ProjectId::from_uuid(uuid::Uuid::from_u128(0xbeef));
        spool_foreign(&harness, neighbour, &harness.scope.scope);

        let report = rebuilt(&harness);

        assert_eq!(report.sessions, 0);
    }

    /// A name renamed away from can be taken up again — a clone without the
    /// new marker resolves it — and its new sessions are filed under the old
    /// directory with the old id. The note belongs to the directory it was
    /// written in, and speaks for nothing outside it.
    #[test]
    fn the_old_project_started_again_elsewhere_is_not_adopted() {
        let harness = harness();
        harness
            .raw
            .record_previous(
                &harness.scope.scope,
                old_project(),
                &scope_named("old-name"),
            )
            .expect("note");
        spool_foreign(&harness, old_project(), &scope_named("old-name"));

        let report = rebuilt(&harness);

        assert_eq!(report.sessions, 0);
    }

    /// The origin lives in the page, so a rebuild restores it, and an index
    /// that lost it — or never had it, having been built before the column
    /// existed — is told apart from one that did not.
    #[test]
    fn an_origin_is_rebuilt_from_its_page_and_checked() {
        let harness = harness();
        let mut frontmatter = Frontmatter::new("Keys live in the store", Vec::new()).expect("fm");
        frontmatter.origin = Some(anamnesis_core::page::Origin::Human);
        frontmatter.quote = Some("every provider reads its key from the store".to_owned());
        let page = Page::new(
            harness.scope.project_id,
            PagePath::parse("decisions/keys.md").expect("path"),
            frontmatter,
            "Every provider reads its key from the credential store.",
        );
        harness
            .wiki
            .write_page(&harness.scope.scope, &page, "write")
            .expect("write");
        rebuilt(&harness);

        let decided = harness
            .store
            .standing_decisions(harness.scope.project_id, 5)
            .expect("decisions");
        assert_eq!(decided[0].origin, Some(anamnesis_core::page::Origin::Human));
        assert!(checked(&harness).is_clean());

        // As the wiki holds it, so that the origin is the only difference.
        let on_disk = harness
            .wiki
            .read_page(&harness.scope.scope, &page.path)
            .expect("read");
        let mut forgotten = Page::new(
            harness.scope.project_id,
            page.path.clone(),
            on_disk.frontmatter,
            on_disk.body,
        );
        forgotten.frontmatter.origin = None;
        harness
            .store
            .upsert_page(&forgotten, now())
            .expect("upsert");

        assert_eq!(
            checked(&harness).drift.pages.differing,
            [("decisions/keys.md".to_owned(), vec!["origin"])]
        );
    }
}
