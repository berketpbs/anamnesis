//! The commands that read and write wiki pages by hand.
//!
//! Memory is written by capture and by consolidation; these are the four ways
//! a person reaches into it directly — searching it, writing a page, reading
//! one back, and forgetting one on purpose. They share one rule: a page a
//! person wrote is not different from a page anamnesis wrote, because the
//! retrieval that answers the next session does not know which is which.

use std::path::PathBuf;

use anamnesis_wiki::Wiki;
use jiff::Timestamp;

use anamnesis_core::audit::Action;

use crate::audit::note;
use crate::project::{global_scope, open_project};

/// Search this project's pages, and the workspace's shared ones.
pub fn cmd_search(
    query: &str,
    limit: usize,
    path: Option<String>,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;

    // The same opt-in local embedder `anamnesis mcp` uses, so a search from
    // the terminal ranks identically to one an agent runs.
    let embedder = anamnesis_llm::EmbedConfig::from_env().build(&data.models())?;
    let query_vector = embedder
        .as_ref()
        .and_then(|embedder| match embedder.embed(query) {
            Ok(vector) => Some((embedder.model().to_owned(), vector)),
            Err(error) => {
                eprintln!("anamnesis: query embedding failed ({error}); searching without it");
                None
            }
        });

    // The workspace's shared scope is searched alongside this project's, so a
    // policy written once is found from every project that inherits it.
    let global = global_scope(&scope, &data);
    let hits = store.query_pages_across(
        scope.project_id,
        &[global.project_id],
        query,
        limit,
        Timestamp::now(),
        query_vector
            .as_ref()
            .map(|(model, vector)| (model.as_str(), vector.as_slice())),
    )?;

    let hits: Vec<_> = match &path {
        Some(prefix) => hits
            .into_iter()
            .filter(|hit| hit.path.as_str().starts_with(prefix.as_str()))
            .collect(),
        None => hits,
    };

    if hits.is_empty() {
        println!("No pages matched {query:?}.");
        return Ok(());
    }

    for hit in &hits {
        let mut marks = Vec::new();
        if hit.pinned {
            marks.push("pinned");
        }
        if hit.canonical {
            marks.push("canonical");
        }
        if !hit.status.is_answerable() {
            marks.push(hit.status.as_str());
        }
        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!(" [{}]", marks.join(", "))
        };

        // Which scope a hit came from is not cosmetic: a policy that applies
        // everywhere and a note about this project are different kinds of
        // answer, and the path alone does not say which is which.
        let from = if hit.project_id == global.project_id {
            format!(" ({})", anamnesis_core::scope::GLOBAL_PROJECT)
        } else {
            String::new()
        };
        println!("{}  {}{}{}", hit.path, hit.title, marks, from);
        println!("    {} · score {:.4}", hit.tier.as_str(), hit.score);
        if !hit.snippet.is_empty() {
            println!("    {}", hit.snippet.replace('\n', " "));
        }
        println!();
    }
    Ok(())
}

/// Everything about a page except what it says.
///
/// A struct rather than eight positional arguments, four of which are `bool`
/// or `Option<String>` and would swap silently.
#[derive(Debug, Default)]
/// Everything `write-page` accepts beyond the path and the body.
pub struct PageOptions {
    /// Exempt from the decay sweep.
    pub pinned: bool,
    /// When the page should be forgotten.
    pub expires_at: Option<String>,
    /// Temporal tier.
    pub tier: Option<String>,
    /// Trust level.
    pub status: Option<String>,
    /// Authoritative on its subject.
    pub canonical: bool,
    /// Canonical names the page is about.
    pub entities: Vec<String>,
    /// Page this one replaces.
    pub supersedes: Option<String>,
    /// Write into the workspace's shared scope rather than this project.
    pub global: bool,
}

/// Write one page by hand, as a person rather than as consolidation.
pub fn cmd_write_page(
    path: &str,
    title: &str,
    body: &str,
    options: PageOptions,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (project, data, store) = open_project(data_dir)?;
    // Resolved from the project either way: the shared scope belongs to the
    // workspace this project is in, so standing somewhere else writes to a
    // different one.
    let scope = if options.global {
        global_scope(&project, &data)
    } else {
        project
    };
    let wiki = Wiki::open(data.wiki())?;

    let page_path = anamnesis_core::page::PagePath::parse(path)?;
    let entities = options
        .entities
        .iter()
        .map(|name| anamnesis_core::page::Entity::parse(name))
        .collect::<anamnesis_core::Result<Vec<_>>>()?;

    let mut frontmatter = anamnesis_core::page::Frontmatter::new(title, entities.clone())?;
    frontmatter.pinned = options.pinned;
    frontmatter.canonical = options.canonical;
    if let Some(tier) = &options.tier {
        frontmatter.tier = anamnesis_core::page::Tier::parse(tier)?;
    }
    if let Some(status) = &options.status {
        frontmatter.status = anamnesis_core::page::PageStatus::parse(status)?;
    }
    if let Some(supersedes) = &options.supersedes {
        frontmatter.supersedes = Some(anamnesis_core::page::PagePath::parse(supersedes)?);
    }
    if let Some(expires) = &options.expires_at {
        // Accepting a bare date is the whole reason this is parsed here rather
        // than deserialized: `--expires-at 2026-12-31` is what someone types,
        // and rejecting it for want of a time of day would be pedantry.
        let stamp = if expires.len() == 10 {
            format!("{expires}T00:00:00Z")
        } else {
            expires.clone()
        };
        frontmatter.expires_at = Some(stamp.parse().map_err(|_| {
            anyhow::anyhow!("--expires-at {expires:?} is not a date or RFC 3339 timestamp")
        })?);
    }

    let now = Timestamp::now();
    store.upsert_project(&scope, now)?;

    let mut page =
        anamnesis_core::page::Page::new(scope.project_id, page_path.clone(), frontmatter, body);
    let commit = wiki.write_page(&scope.scope, &page, &format!("cli: write {page_path}"))?;
    page.git_commit = Some(commit.clone());
    note(
        &store,
        Some(scope.project_id),
        Action::PageWritten,
        page_path.to_string(),
        Some(format!("commit {}", &commit[..commit.len().min(8)])),
    );

    // Entities as well as links, which this command used to skip because it
    // could not set any. A page whose entities never reach the index is one
    // the entity stream cannot find, however carefully they were declared.
    // And a vector, when one is enabled: a page written here is a page
    // somebody meant, and leaving it out of the vector stream would make the
    // stream depend on which command wrote a page rather than on what it says.
    let embedder = anamnesis_llm::EmbedConfig::from_env().build(&data.models())?;
    store.index_page(
        scope.project_id,
        &page,
        &anamnesis_wiki::extract_links(body),
        embedder
            .as_deref()
            .map(|embedder| embedder as &dyn anamnesis_core::embedding::Embed),
        now,
    )?;

    if options.global {
        println!("🌍 Wrote {page_path} to {}", scope.scope);
        println!("   every project in {} searches it", scope.scope.workspace);
    } else {
        println!("✍️  Wrote {page_path}");
    }
    println!("   {}", wiki.locate(&scope.scope, &page_path).display());
    println!("   commit {}", &commit[..commit.len().min(8)]);
    println!("   {}", describe_page(&page.frontmatter));
    if let Some(replaced) = &page.frontmatter.supersedes {
        // Said out loud because it is the one flag that changes another page:
        // whatever it named stops being offered to recall.
        println!("   replaces {replaced}, which recall will stop offering");
    }
    Ok(())
}

/// The one-line summary of what a page was written as.
fn describe_page(frontmatter: &anamnesis_core::page::Frontmatter) -> String {
    let mut parts = vec![frontmatter.tier.as_str().to_owned()];
    if frontmatter.status != anamnesis_core::page::PageStatus::default() {
        parts.push(frontmatter.status.as_str().to_owned());
    }
    if frontmatter.canonical {
        parts.push("canonical".to_owned());
    }
    if frontmatter.pinned {
        parts.push("pinned".to_owned());
    }
    if !frontmatter.entities.is_empty() {
        parts.push(format!(
            "entities: {}",
            frontmatter
                .entities
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.join(" · ")
}

/// Print one page as it is on disk.
pub fn cmd_show_page(path: &str, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let wiki = Wiki::open(data.wiki())?;
    let page_path = anamnesis_core::page::PagePath::parse(path)?;

    if !wiki.exists(&scope.scope, &page_path) {
        anyhow::bail!(
            "no page at {page_path} — looked in {}",
            wiki.locate(&scope.scope, &page_path).display()
        );
    }

    let page = wiki.read_page(&scope.scope, &page_path)?;
    let fm = &page.frontmatter;

    println!("📄 {}", fm.title);
    println!("   {page_path}");
    println!("   {} · {}", fm.tier.as_str(), fm.status.as_str());
    if fm.pinned {
        println!("   pinned");
    }
    if fm.canonical {
        println!("   canonical");
    }
    if let Some(expires) = fm.expires_at {
        println!("   expires {expires}");
    }
    if let Some(replaces) = &fm.supersedes {
        println!("   replaces {replaces}");
    }
    // Worth saying loudly: retrieval stopped offering this page the moment
    // something replaced it, so anyone reading it here found it by name and
    // has no other way to learn that.
    if let Some(replacement) = store.superseded_by(scope.project_id, &page_path)? {
        println!("   ⚠ replaced by {replacement}");
    }
    if !fm.entities.is_empty() {
        let names: Vec<&str> = fm.entities.iter().map(|e| e.as_str()).collect();
        println!("   entities: {}", names.join(", "));
    }
    println!();
    println!("{}", page.body.trim_end());
    Ok(())
}

/// Remove a page somebody named, from the wiki and the index.
///
/// The counterpart to `sweep`, which forgets what decayed. This forgets what
/// was *wrong*: a page written from a bad reply, a note that turned out to be
/// untrue, a duplicate. Until now the only ways out were to wait for decay —
/// which never comes for a pinned or durable page — or to delete the file by
/// hand and hope the watcher was running to notice.
///
/// Deliberately not gated behind `--apply`, unlike the sweep. A sweep proposes
/// a judgement over pages nobody named, and the report is where that judgement
/// is checked; here a person has named the page. What the command owes them
/// instead is to say what it removed and where it went, which the wiki's git
/// history makes answerable.
/// Forget pages on purpose, from both the wiki and the index.
pub fn cmd_forget(paths: &[String], data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let wiki = Wiki::open(data.wiki())?;

    // Every path is resolved before anything is removed. Forgetting two pages
    // and then refusing the third would leave the caller to work out which of
    // the three names was the typo.
    let mut doomed = Vec::with_capacity(paths.len());
    for path in paths {
        let page_path = anamnesis_core::page::PagePath::parse(path)?;
        if !wiki.exists(&scope.scope, &page_path) {
            anyhow::bail!(
                "no page at {page_path} — looked in {}",
                wiki.locate(&scope.scope, &page_path).display()
            );
        }
        let title = wiki
            .read_page(&scope.scope, &page_path)
            .map(|page| page.frontmatter.title)
            // A page whose frontmatter no longer parses is exactly the kind
            // worth removing, so it is described by its path and removed.
            .unwrap_or_else(|_| "(unreadable frontmatter)".to_owned());
        doomed.push((page_path, title));
    }

    println!("🗑  Forgetting from {}", scope.scope);
    println!();
    for (path, title) in &doomed {
        println!("  {path}");
        println!("     {title}");
    }
    println!();

    // Index first, then the wiki — the order `sweep` chose, for its reason: an
    // interruption here leaves a page briefly unfindable and wholly
    // recoverable by `reindex`, where the reverse order leaves the index
    // pointing at markdown that is gone and no rebuild can repair.
    let mut rows = 0;
    for (path, _) in &doomed {
        if store.delete_page(anamnesis_core::ids::PageId::derive(scope.project_id, path))? {
            rows += 1;
        }
    }

    let removed: Vec<anamnesis_core::page::PagePath> =
        doomed.iter().map(|(path, _)| path.clone()).collect();
    let message = forget_commit_message(&doomed);
    let commit = wiki
        .delete_pages(&scope.scope, &removed, &message)
        .map_err(|error| {
            anyhow::anyhow!(
                "{error}\n\
                 {rows} index row(s) were already dropped, but the wiki still holds every page. \
                 Nothing is lost: run `anamnesis reindex` to put the index back."
            )
        })?;

    for (path, _) in &doomed {
        note(
            &store,
            Some(scope.project_id),
            Action::PageForgotten,
            path.to_string(),
            commit
                .as_ref()
                .map(|commit| format!("commit {}", &commit[..commit.len().min(8)])),
        );
    }

    println!("  {} page(s), {rows} index row(s).", doomed.len());
    match commit {
        Some(commit) => {
            println!("  Committed {}.", &commit[..commit.len().min(8)]);
            println!();
            println!("  Still recoverable — the wiki is a git repository:");
            println!("    git -C {} show {commit}", data.wiki().display());
        }
        None => println!("  Nothing for git to record."),
    }
    Ok(())
}

/// What the wiki's history says about a deliberate removal.
///
/// Named pages and a person's decision, rather than the sweep's decay scores:
/// once the pages are gone this message is the only remaining account of what
/// was here, and "someone decided" is the part that would otherwise be lost.
fn forget_commit_message(doomed: &[(anamnesis_core::page::PagePath, String)]) -> String {
    let mut message = format!("forget: {} page(s) removed on request\n", doomed.len());
    for (path, title) in doomed {
        message.push_str(&format!("\n- {path} — {title}"));
    }
    message.push('\n');
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The summary line is the only feedback that a flag was understood, so
    /// every flag that changes how a page is treated has to appear in it.
    #[test]
    fn the_summary_names_what_the_page_was_written_as() {
        let mut frontmatter =
            anamnesis_core::page::Frontmatter::new("t", Vec::new()).expect("frontmatter");
        assert_eq!(describe_page(&frontmatter), "episodic");

        frontmatter.tier = anamnesis_core::page::Tier::Semantic;
        frontmatter.canonical = true;
        frontmatter.pinned = true;
        frontmatter.status = anamnesis_core::page::PageStatus::Historical;
        frontmatter.entities = vec![
            anamnesis_core::page::Entity::parse("SQLite").expect("entity"),
            anamnesis_core::page::Entity::parse("recall").expect("entity"),
        ];

        let line = describe_page(&frontmatter);
        assert!(line.contains("semantic"), "{line}");
        assert!(line.contains("historical"), "{line}");
        assert!(line.contains("canonical"), "{line}");
        assert!(line.contains("pinned"), "{line}");
        assert!(line.contains("SQLite, recall"), "{line}");
    }

    /// The default status is the ordinary case and saying it adds nothing;
    /// every other status changes whether an agent answers from the page.
    #[test]
    fn an_ordinary_page_is_described_by_its_tier_alone() {
        let frontmatter =
            anamnesis_core::page::Frontmatter::new("t", Vec::new()).expect("frontmatter");
        assert!(!describe_page(&frontmatter).contains("active"));
    }

    /// Once the pages are gone this message is the only account of what was
    /// here, so it has to carry both halves: which pages, and that a person
    /// asked for it rather than a decay score deciding.
    #[test]
    fn the_forget_commit_names_every_page_and_says_who_asked() {
        let doomed = vec![
            (
                anamnesis_core::page::PagePath::parse("notes/wrong.md").expect("path"),
                "A note that turned out to be untrue".to_owned(),
            ),
            (
                anamnesis_core::page::PagePath::parse("sessions/2026-08-29-abcd.md").expect("path"),
                "2026-08-29: a session".to_owned(),
            ),
        ];

        let message = forget_commit_message(&doomed);

        assert!(message.starts_with("forget: 2 page(s) removed on request"));
        assert!(
            message.contains("notes/wrong.md — A note that turned out"),
            "{message}"
        );
        assert!(
            message.contains("sessions/2026-08-29-abcd.md — 2026-08-29: a session"),
            "{message}"
        );
    }
}
