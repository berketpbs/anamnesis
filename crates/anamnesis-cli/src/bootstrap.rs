//! Seeding a project's memory from the one record it already has: its git
//! history.
//!
//! A new project starts with an empty wiki, and stays empty until enough
//! sessions have been consolidated to say anything. But the repository has
//! been recording who worked on what, and where the work concentrated, since
//! its first commit. Bootstrap reads that record and writes it down as pages,
//! so the first agent session in a repository starts with an answer to "what
//! is this, and which parts move" instead of nothing.
//!
//! Three properties this is built around:
//!
//! * **Derived, never decided.** Everything here is a mechanical summary of
//!   commits. Bootstrap pages therefore live in their own `bootstrap/`
//!   namespace, carry below-default salience, and never claim an authority
//!   namespace (`decisions/`, `_rules/`, ...) — a churn count must not outrank
//!   something a session actually learned.
//! * **An existing page is never overwritten.** Bootstrap seeds; it does not
//!   maintain. A page at one of these paths may have been rewritten by hand
//!   or superseded by real work, and clobbering that would make the command
//!   dangerous to re-run. `force` overrides this, and is the only way to
//!   refresh a stale snapshot.
//! * **Bounded work.** Churn is computed by diffing each commit against its
//!   parent, which on a large repository is the difference between a second
//!   and several minutes. The walk stops after [`DEFAULT_MAX_COMMITS`], and
//!   the pages say so rather than presenting a partial count as total.
//!
//! Author emails are deliberately dropped. They are in the repository
//! already, but a memory system is read back by a model and copied into
//! summaries; names identify a contributor for every purpose this serves.

use anamnesis_core::page::{Entity, Frontmatter, Page, PagePath, Tier};
use anamnesis_core::scope::ResolvedScope;
use anamnesis_store::Store;
use anamnesis_wiki::Wiki;
use jiff::Timestamp;
use std::collections::HashMap;
use std::path::Path;

/// How many commits a survey walks before it stops and says it stopped.
pub const DEFAULT_MAX_COMMITS: usize = 1_000;

/// Contributors listed on the contributors page.
const TOP_AUTHORS: usize = 12;
/// Files listed on the hotspots page.
const TOP_HOTSPOTS: usize = 20;
/// Commits listed on the recent-work page.
const RECENT_COMMITS: usize = 25;
/// File extensions listed as the repository's languages.
const TOP_EXTENSIONS: usize = 8;
/// Top-level directories listed as the repository's layout.
const TOP_DIRECTORIES: usize = 12;
/// Longest commit summary reproduced on a page, in characters.
const MAX_SUMMARY: usize = 120;
/// Most entity candidates taken from one list.
const ENTITY_CANDIDATES: usize = 8;

/// One contributor's footprint in the walked history.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Author {
    /// Name as recorded on the commits, with table-breaking characters escaped.
    pub name: String,
    /// Commits authored within the walk.
    pub commits: usize,
    /// Earliest commit seen.
    pub first: Timestamp,
    /// Latest commit seen.
    pub last: Timestamp,
}

/// One file's churn in the walked history.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hotspot {
    /// Path as recorded in the tree.
    pub path: String,
    /// Non-merge commits that touched it.
    pub changes: usize,
    /// When it was last touched.
    pub last: Timestamp,
}

/// One commit, as reproduced on the recent-work page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Abbreviated object id.
    pub short_id: String,
    /// Author name.
    pub author: String,
    /// Commit summary, escaped and truncated.
    pub summary: String,
    /// Author time.
    pub at: Timestamp,
}

/// What reading the repository found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Survey {
    /// Branch `HEAD` points at, if it is not detached.
    pub branch: Option<String>,
    /// Abbreviated `HEAD` object id.
    pub head: Option<String>,
    /// URL of the `origin` remote, if there is one.
    pub remote: Option<String>,
    /// Commits walked.
    pub commits: usize,
    /// Merge commits among them, excluded from churn.
    pub merges: usize,
    /// Whether the walk hit its bound before running out of history.
    pub truncated: bool,
    /// Author time of the oldest commit walked.
    pub first: Option<Timestamp>,
    /// Author time of the newest commit walked.
    pub last: Option<Timestamp>,
    /// Contributors, most commits first.
    pub authors: Vec<Author>,
    /// Most-changed files, most changes first.
    pub hotspots: Vec<Hotspot>,
    /// Newest commits, newest first.
    pub recent: Vec<Commit>,
    /// Tag names, as git reports them.
    pub tags: Vec<String>,
    /// File extensions in the `HEAD` tree, by file count.
    pub extensions: Vec<(String, usize)>,
    /// Top-level directories in the `HEAD` tree, by file count.
    pub directories: Vec<(String, usize)>,
    /// Files sitting directly in the repository root.
    pub root_files: usize,
}

impl Survey {
    /// Whether there is any history to write pages from.
    pub fn is_empty(&self) -> bool {
        self.commits == 0
    }
}

/// A page bootstrap would write.
#[derive(Debug, Clone)]
pub struct Draft {
    /// Where it goes in the wiki.
    pub path: PagePath,
    /// Its frontmatter.
    pub frontmatter: Frontmatter,
    /// Its markdown body.
    pub body: String,
}

/// What seeding did.
#[derive(Debug, Clone, Default)]
pub struct Seeded {
    /// Pages written.
    pub written: Vec<PagePath>,
    /// Pages left alone because something was already there.
    pub skipped: Vec<PagePath>,
}

/// Read a repository's history.
///
/// `max_commits` bounds the walk. Merges are counted but not diffed: a merge's
/// diff against its first parent reattributes every file the branch touched to
/// the merge, which would put whoever merges most at the top of the hotspots.
pub fn survey(repo_path: &Path, max_commits: usize) -> anyhow::Result<Survey> {
    let repo = git2::Repository::discover(repo_path)?;
    let mut survey = Survey::default();

    if let Ok(remote) = repo.find_remote("origin") {
        survey.remote = remote.url().ok().map(sanitize_cell);
    }
    if let Ok(names) = repo.tag_names(None) {
        // Each entry is fallible twice over: a tag slot can be empty, and a
        // name can be bytes that are not UTF-8. Neither is worth failing a
        // survey for, so both are dropped.
        survey.tags = names
            .iter()
            .filter_map(|name| name.ok().flatten())
            .map(sanitize_cell)
            .collect();
    }

    // No HEAD is an unborn branch: a repository someone has only just run
    // `git init` in. Nothing to survey, and not an error.
    let Ok(head) = repo.head() else {
        return Ok(survey);
    };
    survey.branch = head.shorthand().ok().map(sanitize_cell);
    let head_commit = head.peel_to_commit()?;
    survey.head = Some(short_id(head_commit.id()));
    let files = tree_files(&head_commit)?;
    survey.extensions = extension_counts(&files);
    let (directories, root_files) = directory_counts(&files);
    survey.directories = directories;
    survey.root_files = root_files;

    let mut walk = repo.revwalk()?;
    walk.push_head()?;
    walk.set_sorting(git2::Sort::TIME)?;

    let mut authors: HashMap<String, Author> = HashMap::new();
    let mut churn: HashMap<String, Hotspot> = HashMap::new();

    for oid in walk.by_ref().take(max_commits) {
        let commit = repo.find_commit(oid?)?;
        let at = commit_time(&commit);
        survey.commits += 1;

        let name = commit
            .author()
            .name()
            .map_or_else(|_| "(unknown)".to_owned(), sanitize_cell);
        authors
            .entry(name.clone())
            .and_modify(|author| {
                author.commits += 1;
                author.first = author.first.min(at);
                author.last = author.last.max(at);
            })
            .or_insert_with(|| Author {
                name: name.clone(),
                commits: 1,
                first: at,
                last: at,
            });

        survey.first = Some(survey.first.map_or(at, |first| first.min(at)));
        survey.last = Some(survey.last.map_or(at, |last| last.max(at)));

        if survey.recent.len() < RECENT_COMMITS {
            survey.recent.push(Commit {
                short_id: short_id(commit.id()),
                author: name,
                summary: commit
                    .summary()
                    .ok()
                    .flatten()
                    .map_or_else(|| "(no summary)".to_owned(), truncated_cell),
                at,
            });
        }

        if commit.parent_count() > 1 {
            survey.merges += 1;
            continue;
        }
        for path in touched_paths(&repo, &commit)? {
            churn
                .entry(path.clone())
                .and_modify(|hotspot| {
                    hotspot.changes += 1;
                    hotspot.last = hotspot.last.max(at);
                })
                .or_insert(Hotspot {
                    path,
                    changes: 1,
                    last: at,
                });
        }
    }

    // Anything left in the walk means the bound stopped it, not the history.
    survey.truncated = walk.next().is_some();

    survey.authors = rank(authors.into_values(), |author| {
        (author.commits, author.last)
    });
    survey.authors.truncate(TOP_AUTHORS);
    survey.hotspots = rank(churn.into_values(), |hotspot| {
        (hotspot.changes, hotspot.last)
    });
    survey.hotspots.truncate(TOP_HOTSPOTS);

    Ok(survey)
}

/// Render the pages a survey supports.
///
/// Pure: no repository, no wiki, and no clock beyond the `now` it is handed.
/// The only reason a draft fails to build is a page path or entity this
/// module spelled wrong, which a test catches rather than a user.
pub fn draft(survey: &Survey, now: Timestamp) -> anyhow::Result<Vec<Draft>> {
    if survey.is_empty() {
        return Ok(Vec::new());
    }

    let mut drafts = vec![
        page(
            "bootstrap/repository.md",
            "Repository overview",
            Tier::Semantic,
            1.0,
            entities(survey.extensions.iter().take(4).map(|(ext, _)| ext.clone())),
            render_repository(survey, now),
        )?,
        page(
            "bootstrap/contributors.md",
            "Contributors",
            Tier::Semantic,
            0.7,
            entities(survey.authors.iter().map(|author| author.name.clone())),
            render_contributors(survey, now),
        )?,
    ];

    if !survey.hotspots.is_empty() {
        drafts.push(page(
            "bootstrap/hotspots.md",
            "Where the work concentrates",
            Tier::Semantic,
            0.8,
            entities(top_level_names(&survey.hotspots)),
            render_hotspots(survey, now),
        )?);
    }

    drafts.push(page(
        "bootstrap/recent-work.md",
        "Recent commits at bootstrap",
        // Episodic, and the lowest salience of the four: this is the one page
        // here that is stale the moment the next commit lands.
        Tier::Episodic,
        0.5,
        Vec::new(),
        render_recent(survey, now),
    )?);

    Ok(drafts)
}

/// Write the drafts to the wiki and index them.
///
/// Links are resolved in a second pass, after every page exists in the index.
/// These pages link to each other, and a single pass would leave whichever was
/// written first pointing at a target the index had not seen yet — the same
/// dangling-link failure a rebuild has to avoid.
pub fn seed(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    drafts: &[Draft],
    force: bool,
    embedder: Option<&dyn anamnesis_core::embedding::Embed>,
    now: Timestamp,
) -> anyhow::Result<Seeded> {
    let mut report = Seeded::default();
    store.upsert_project(scope, now)?;
    let mut indexed = Vec::new();

    for item in drafts {
        if !force && wiki.exists(&scope.scope, &item.path) {
            report.skipped.push(item.path.clone());
            continue;
        }

        let entities = item.frontmatter.entities.clone();
        let mut page = Page::new(
            scope.project_id,
            item.path.clone(),
            item.frontmatter.clone(),
            item.body.clone(),
        );
        let commit = wiki.write_page(
            &scope.scope,
            &page,
            &format!("bootstrap: {}", item.frontmatter.title),
        )?;
        page.git_commit = Some(commit);

        store.upsert_page(&page, now)?;
        store.set_page_entities(scope.project_id, page.id, &entities)?;
        store.embed_page(&page, embedder)?;
        indexed.push((page.id, item.body.clone()));
        report.written.push(item.path.clone());
    }

    for (page_id, body) in &indexed {
        store.set_page_links(
            scope.project_id,
            *page_id,
            &anamnesis_wiki::extract_links(body),
        )?;
    }

    Ok(report)
}

/// Assemble one draft, validating its path and entities.
fn page(
    path: &str,
    title: &str,
    tier: Tier,
    salience: f64,
    entities: Vec<Entity>,
    body: String,
) -> anyhow::Result<Draft> {
    let mut frontmatter = Frontmatter::new(title, entities)?;
    frontmatter.tier = tier;
    frontmatter.salience = salience;
    Ok(Draft {
        path: PagePath::parse(path)?,
        frontmatter,
        body,
    })
}

/// Identity, scale, and shape of the repository.
fn render_repository(survey: &Survey, now: Timestamp) -> String {
    let mut out = String::new();
    out.push_str("## Identity\n\n");
    row(
        &mut out,
        "Remote",
        survey.remote.as_deref().unwrap_or("(none)"),
    );
    row(
        &mut out,
        "Branch",
        survey.branch.as_deref().unwrap_or("(detached)"),
    );
    row(&mut out, "HEAD", survey.head.as_deref().unwrap_or("(none)"));

    out.push_str("\n## History\n\n");
    let span = match (survey.first, survey.last) {
        (Some(first), Some(last)) => format!("{} to {}", day(first), day(last)),
        _ => "(unknown)".to_owned(),
    };
    row(&mut out, "Commits walked", &survey.commits.to_string());
    row(&mut out, "Span", &span);
    row(&mut out, "Merges", &survey.merges.to_string());
    row(&mut out, "Contributors", &survey.authors.len().to_string());
    if survey.truncated {
        out.push_str(
            "\nThe walk stopped at its bound, so the numbers above describe the most\nrecent commits rather than the whole history.\n",
        );
    }

    if !survey.extensions.is_empty() {
        // "Extensions", not "languages": a real repository answers with
        // `.lock`, `.example`, and `.dev`, none of which anyone writes in.
        out.push_str("\n## Files by extension\n\n");
        for (ext, count) in &survey.extensions {
            out.push_str(&format!("- `.{ext}` — {count}\n"));
        }
    }
    if !survey.directories.is_empty() || survey.root_files > 0 {
        out.push_str("\n## Top-level layout\n\n");
        for (dir, count) in &survey.directories {
            out.push_str(&format!("- `{dir}` — {count} file(s)\n"));
        }
        if survey.root_files > 0 {
            out.push_str(&format!(
                "- {} file(s) directly in the repository root\n",
                survey.root_files
            ));
        }
    }
    if !survey.tags.is_empty() {
        out.push_str("\n## Tags\n\n");
        let shown: Vec<&str> = survey.tags.iter().take(10).map(String::as_str).collect();
        out.push_str(&shown.join(", "));
        out.push('\n');
    }

    out.push_str(
        "\nSee also [[bootstrap/contributors]], [[bootstrap/hotspots]], and [[bootstrap/recent-work]].\n",
    );
    out.push_str(&provenance(now));
    out
}

/// Who has worked here, and when.
fn render_contributors(survey: &Survey, now: Timestamp) -> String {
    let mut out = String::new();
    out.push_str(
        "Who has committed to this repository, within the commits bootstrap walked.\nCommit counts measure participation, not authorship of any particular\ndecision - read them as \"ask this person\", not \"this person owns it\".\n\n",
    );
    out.push_str("| Contributor | Commits | First | Last |\n");
    out.push_str("| --- | ---: | --- | --- |\n");
    for author in &survey.authors {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            author.name,
            author.commits,
            day(author.first),
            day(author.last)
        ));
    }
    out.push_str("\nSee also [[bootstrap/repository]].\n");
    out.push_str(&provenance(now));
    out
}

/// Where changes have landed most often.
fn render_hotspots(survey: &Survey, now: Timestamp) -> String {
    let mut out = String::new();
    out.push_str(
        "Files touched by the most commits. High churn marks where the work has\nbeen, which is usually where the next change lands too - it is not a\nquality judgement about the file.\n\n",
    );
    out.push_str("| File | Commits | Last touched |\n");
    out.push_str("| --- | ---: | --- |\n");
    for hotspot in &survey.hotspots {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            hotspot.path,
            hotspot.changes,
            day(hotspot.last)
        ));
    }
    out.push_str(
        "\nMerge commits are excluded, so a file is counted once per change rather\nthan again for every merge that carried it.\n",
    );
    out.push_str("\nSee also [[bootstrap/repository]].\n");
    out.push_str(&provenance(now));
    out
}

/// The newest commits, as an episodic snapshot.
fn render_recent(survey: &Survey, now: Timestamp) -> String {
    let mut out = String::new();
    out.push_str("The newest commits at the moment memory was bootstrapped.\n\n");
    for commit in &survey.recent {
        out.push_str(&format!(
            "- `{}` {} — {} ({})\n",
            commit.short_id,
            day(commit.at),
            commit.summary,
            commit.author
        ));
    }
    out.push_str("\nSee also [[bootstrap/repository]].\n");
    out.push_str(&provenance(now));
    out
}

/// The footer every bootstrap page carries.
///
/// Provenance is written into the body, not left to the frontmatter, because
/// the page is read back as text by a model that has no reason to know a
/// `bootstrap/` path means "counted, not concluded".
fn provenance(now: Timestamp) -> String {
    format!(
        "\n---\n\nDerived from git history by `anamnesis bootstrap` on {}. A snapshot of what\nthe commits say, not a decision anyone made; re-run with `--force` to refresh it.\n",
        day(now)
    )
}

/// One labelled fact.
fn row(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("- **{label}:** {value}\n"));
}

/// Date part of a timestamp, in UTC.
fn day(at: Timestamp) -> String {
    let stamp = at.to_string();
    stamp.split('T').next().unwrap_or("undated").to_owned()
}

/// Abbreviated object id, in the length git itself tends to print.
fn short_id(oid: git2::Oid) -> String {
    oid.to_string().chars().take(8).collect()
}

/// Author time of a commit.
///
/// A commit whose timestamp will not convert is dated to the epoch rather than
/// failing the survey: git accepts times this type does not, and one absurd
/// date is not worth losing the history around it.
fn commit_time(commit: &git2::Commit<'_>) -> Timestamp {
    Timestamp::from_second(commit.author().when().seconds()).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// Paths a non-merge commit touched.
fn touched_paths(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
) -> anyhow::Result<Vec<String>> {
    let tree = commit.tree()?;
    // The root commit has no parent: everything in it is new.
    let parent = match commit.parent(0) {
        Ok(parent) => Some(parent.tree()?),
        Err(_) => None,
    };
    let diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&tree), None)?;

    let mut paths = Vec::new();
    for delta in diff.deltas() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        if let Some(path) = path {
            paths.push(sanitize_cell(&path));
        }
    }
    Ok(paths)
}

/// File extensions among a set of paths, most files first.
fn extension_counts(files: &[String]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for path in files {
        let name = path.rsplit('/').next().unwrap_or(path);
        // A leading dot is a name, not an extension: `.gitignore` is not
        // 4,000 files' worth of the "gitignore" language.
        let Some((stem, ext)) = name.rsplit_once('.') else {
            continue;
        };
        if !stem.is_empty()
            && !ext.is_empty()
            && ext.len() <= 12
            && ext.chars().all(char::is_alphanumeric)
        {
            *counts.entry(ext.to_lowercase()).or_default() += 1;
        }
    }
    let mut ranked = rank(counts.into_iter(), |(_, count)| *count);
    ranked.truncate(TOP_EXTENSIONS);
    ranked
}

/// Top-level directories among a set of paths, and how many files sit in the
/// root itself.
///
/// Root files are counted rather than listed: mixing them into the same list
/// buries the directories that describe the layout under a dozen one-file
/// entries for `README.md`, `.gitignore`, and friends.
fn directory_counts(files: &[String]) -> (Vec<(String, usize)>, usize) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut root = 0;
    for path in files {
        match path.split_once('/') {
            Some((head, _)) => *counts.entry(format!("{head}/")).or_default() += 1,
            None => root += 1,
        }
    }
    let mut ranked = rank(counts.into_iter(), |(_, count)| *count);
    ranked.truncate(TOP_DIRECTORIES);
    (ranked, root)
}

/// Every blob path in a commit's tree.
fn tree_files(commit: &git2::Commit<'_>) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    commit
        .tree()?
        .walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob)
                && let Ok(name) = entry.name()
            {
                files.push(format!("{root}{name}"));
            }
            git2::TreeWalkResult::Ok
        })?;
    Ok(files)
}

/// Sort descending by a key, breaking ties on the values themselves.
///
/// The tiebreak matters: a `HashMap` yields its entries in an order that
/// varies between runs, and two files with the same churn would otherwise
/// swap places every time, producing a wiki commit that changes nothing.
fn rank<T, K, F>(items: impl Iterator<Item = T>, key: F) -> Vec<T>
where
    T: Ord,
    K: Ord,
    F: Fn(&T) -> K,
{
    let mut items: Vec<T> = items.collect();
    items.sort_by(|a, b| key(b).cmp(&key(a)).then_with(|| a.cmp(b)));
    items
}

/// Turn names into entities, dropping any the core rejects.
fn entities(names: impl Iterator<Item = String>) -> Vec<Entity> {
    names
        .filter_map(|name| Entity::parse(&name).ok())
        .take(ENTITY_CANDIDATES)
        .collect()
}

/// Distinct top-level names among the hotspots, as entity candidates.
fn top_level_names(hotspots: &[Hotspot]) -> impl Iterator<Item = String> {
    let mut seen: Vec<String> = Vec::new();
    for hotspot in hotspots {
        let top = hotspot
            .path
            .split_once('/')
            .map_or(hotspot.path.as_str(), |(head, _)| head)
            .to_owned();
        if !seen.contains(&top) {
            seen.push(top);
        }
    }
    seen.into_iter()
}

/// Make a value safe to drop into a markdown table cell.
fn sanitize_cell(value: &str) -> String {
    let collapsed: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    collapsed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// Sanitize and bound a commit summary.
fn truncated_cell(value: &str) -> String {
    let clean = sanitize_cell(value);
    if clean.chars().count() <= MAX_SUMMARY {
        return clean;
    }
    let head: String = clean.chars().take(MAX_SUMMARY).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::scope::resolve_scope;
    use std::path::PathBuf;

    /// A repository built commit by commit, so a test states the history it
    /// depends on instead of borrowing whatever this project's own history
    /// happens to look like today.
    struct Fixture {
        dir: tempfile::TempDir,
        repo: git2::Repository,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("repo dir");
            let repo = git2::Repository::init(dir.path()).expect("git init");
            Self { dir, repo }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        /// Commit a set of files, authored by `who` at `when` (epoch seconds).
        fn commit(&self, files: &[(&str, &str)], message: &str, who: &str, when: i64) -> git2::Oid {
            for (name, body) in files {
                let full: PathBuf = self.path().join(name);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).expect("mkdir");
                }
                std::fs::write(&full, body).expect("write");
            }
            let tree = self.stage(files);

            let when = git2::Time::new(when, 0);
            let signature =
                git2::Signature::new(who, "dev@example.invalid", &when).expect("signature");
            let head = self
                .repo
                .head()
                .ok()
                .and_then(|head| head.peel_to_commit().ok());
            let parents: Vec<&git2::Commit<'_>> = head.iter().collect();

            self.repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    message,
                    &tree,
                    &parents,
                )
                .expect("commit")
        }

        /// Record a two-parent commit, the shape churn has to ignore.
        fn merge(&self, files: &[(&str, &str)], message: &str, when: i64) -> git2::Oid {
            for (name, body) in files {
                std::fs::write(self.path().join(name), body).expect("write");
            }
            let tree = self.stage(files);

            let head = self
                .repo
                .head()
                .expect("head")
                .peel_to_commit()
                .expect("commit");
            let older = head.parent(0).expect("parent");
            let when = git2::Time::new(when, 0);
            let signature =
                git2::Signature::new("Merger", "dev@example.invalid", &when).expect("signature");

            self.repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    message,
                    &tree,
                    &[&head, &older],
                )
                .expect("merge commit")
        }

        /// Stage the named files and return the tree they make.
        fn stage(&self, files: &[(&str, &str)]) -> git2::Tree<'_> {
            let mut index = self.repo.index().expect("index");
            for (name, _) in files {
                index.add_path(Path::new(name)).expect("add");
            }
            index.write().expect("index write");
            let tree_id = index.write_tree().expect("write tree");
            self.repo.find_tree(tree_id).expect("tree")
        }
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    /// 2026-01-01T00:00:00Z as epoch seconds, plus `days`.
    fn at(days: i64) -> i64 {
        1_767_225_600 + days * 86_400
    }

    fn body_of<'a>(pages: &'a [Draft], path: &str) -> &'a str {
        pages
            .iter()
            .find(|page| page.path.as_str() == path)
            .map_or("", |page| page.body.as_str())
    }

    #[test]
    fn a_survey_counts_commits_authors_and_churn() {
        let fixture = Fixture::new();
        fixture.commit(&[("src/lib.rs", "one")], "feat: start", "Ada", at(0));
        fixture.commit(&[("src/lib.rs", "two")], "fix: again", "Ada", at(1));
        fixture.commit(&[("README.md", "docs")], "docs: readme", "Grace", at(2));

        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");

        assert_eq!(survey.commits, 3);
        assert_eq!(survey.merges, 0);
        assert!(!survey.truncated);
        assert_eq!(survey.authors.len(), 2);
        // Ranked by commits, so the two-commit author leads.
        assert_eq!(survey.authors[0].name, "Ada");
        assert_eq!(survey.authors[0].commits, 2);
        assert_eq!(survey.hotspots[0].path, "src/lib.rs");
        assert_eq!(survey.hotspots[0].changes, 2);
        // Newest first, and every commit is described.
        assert_eq!(survey.recent.len(), 3);
        assert_eq!(survey.recent[0].summary, "docs: readme");
    }

    #[test]
    fn an_empty_repository_yields_nothing_to_seed() {
        // `git init` and nothing else: an unborn HEAD must read as "no
        // history", not as an error that stops someone bootstrapping a
        // project they have only just created.
        let fixture = Fixture::new();

        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");

        assert!(survey.is_empty());
        assert!(draft(&survey, now()).expect("draft").is_empty());
    }

    #[test]
    fn merge_commits_are_counted_but_not_blamed_for_churn() {
        // A merge's diff against its first parent contains every file the
        // branch touched. Counting those would credit whoever merges most
        // with having changed everything.
        let fixture = Fixture::new();
        fixture.commit(&[("a.txt", "one")], "feat: a", "Ada", at(0));
        fixture.commit(&[("b.txt", "two")], "feat: b", "Ada", at(1));
        fixture.merge(&[("merged.txt", "merged")], "merge branch", at(2));

        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");

        assert_eq!(survey.commits, 3);
        assert_eq!(survey.merges, 1);
        assert!(
            !survey
                .hotspots
                .iter()
                .any(|hotspot| hotspot.path == "merged.txt"),
            "a merge must not create churn: {:?}",
            survey.hotspots
        );
    }

    #[test]
    fn the_walk_stops_at_its_bound_and_says_so() {
        let fixture = Fixture::new();
        for day in 0..4 {
            fixture.commit(&[("a.txt", "x")], "chore: touch", "Ada", at(day));
        }

        let survey = survey(fixture.path(), 2).expect("survey");

        assert_eq!(survey.commits, 2);
        assert!(survey.truncated);
        let pages = draft(&survey, now()).expect("draft");
        assert!(
            body_of(&pages, "bootstrap/repository.md").contains("stopped at its bound"),
            "a partial count must not be presented as the whole history"
        );
    }

    #[test]
    fn a_pipe_in_a_commit_summary_cannot_break_a_table() {
        let fixture = Fixture::new();
        fixture.commit(
            &[("a.txt", "x")],
            "fix: handle a | b in the parser",
            "A | B",
            at(0),
        );

        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");

        assert_eq!(survey.authors[0].name, r"A \| B");
        assert!(survey.recent[0].summary.contains(r"a \| b"));
    }

    #[test]
    fn drafts_carry_provenance_and_link_to_each_other() {
        let fixture = Fixture::new();
        fixture.commit(&[("src/lib.rs", "one")], "feat: start", "Ada", at(0));

        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");
        let pages = draft(&survey, now()).expect("draft");

        let paths: Vec<&str> = pages.iter().map(|page| page.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "bootstrap/repository.md",
                "bootstrap/contributors.md",
                "bootstrap/hotspots.md",
                "bootstrap/recent-work.md",
            ]
        );

        for page in &pages {
            assert!(
                page.body.contains("Derived from git history"),
                "{} lost its provenance footer",
                page.path
            );
            // Nothing counted from commits may sit in a namespace that
            // outranks pages someone reasoned their way to.
            assert!(!page.path.is_authoritative());
            assert!(page.frontmatter.salience <= 1.0);
        }

        let overview = pages.first().expect("overview");
        let links = anamnesis_wiki::extract_links(&overview.body);
        assert!(links.contains(&"bootstrap/contributors".to_owned()));
        assert!(links.contains(&"bootstrap/hotspots".to_owned()));
    }

    /// A wiki, an index, and a scope, as `seed` expects them.
    struct Memory {
        _data: tempfile::TempDir,
        store: Store,
        wiki: Wiki,
        scope: ResolvedScope,
    }

    fn memory(repo: &Path) -> Memory {
        std::fs::write(
            repo.join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("marker");
        let scope = resolve_scope(repo).expect("scope");

        let data = tempfile::tempdir().expect("data");
        let store = Store::open(data.path().join("index.db")).expect("store");
        store.migrate().expect("migrate");
        let wiki = Wiki::open(data.path().join("wiki")).expect("wiki");

        Memory {
            _data: data,
            store,
            wiki,
            scope,
        }
    }

    #[test]
    fn seeding_writes_pages_and_indexes_them() {
        let fixture = Fixture::new();
        fixture.commit(
            &[("src/parser.rs", "one")],
            "feat: parse frontmatter",
            "Ada",
            at(0),
        );
        let memory = memory(fixture.path());
        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");
        let drafts = draft(&survey, now()).expect("draft");

        let report = seed(
            &memory.store,
            &memory.wiki,
            &memory.scope,
            &drafts,
            false,
            None,
            now(),
        )
        .expect("seed");

        assert_eq!(report.written.len(), drafts.len());
        assert!(report.skipped.is_empty());
        // On disk...
        for path in &report.written {
            assert!(memory.wiki.exists(&memory.scope.scope, path));
        }
        // ...and answerable, which is the half a wiki write alone would miss.
        let hits = memory
            .store
            .query_pages(memory.scope.project_id, "contributors", 10, now(), None)
            .expect("query");
        assert!(
            hits.iter()
                .any(|hit| hit.path.as_str() == "bootstrap/contributors.md"),
            "seeded pages must be searchable immediately: {hits:?}"
        );
    }

    #[test]
    fn seeding_twice_leaves_an_edited_page_alone() {
        // The case this protects: someone rewrites bootstrap/repository.md by
        // hand, then a later session runs bootstrap again. Losing that edit
        // would make the command unsafe to re-run.
        let fixture = Fixture::new();
        fixture.commit(&[("a.txt", "x")], "feat: start", "Ada", at(0));
        let memory = memory(fixture.path());
        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");
        let drafts = draft(&survey, now()).expect("draft");

        seed(
            &memory.store,
            &memory.wiki,
            &memory.scope,
            &drafts,
            false,
            None,
            now(),
        )
        .expect("first");

        let path = drafts[0].path.clone();
        let edited = memory.wiki.locate(&memory.scope.scope, &path);
        let handwritten =
            std::fs::read_to_string(&edited).expect("read") + "\nHand-written note.\n";
        std::fs::write(&edited, &handwritten).expect("edit");

        let second = seed(
            &memory.store,
            &memory.wiki,
            &memory.scope,
            &drafts,
            false,
            None,
            now(),
        )
        .expect("second");

        assert!(second.written.is_empty());
        assert_eq!(second.skipped.len(), drafts.len());
        assert_eq!(
            std::fs::read_to_string(&edited).expect("read"),
            handwritten,
            "an existing page must survive a re-run untouched"
        );
    }

    #[test]
    fn force_rewrites_what_bootstrap_wrote() {
        let fixture = Fixture::new();
        fixture.commit(&[("a.txt", "x")], "feat: start", "Ada", at(0));
        let memory = memory(fixture.path());
        let survey = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("survey");
        let drafts = draft(&survey, now()).expect("draft");

        seed(
            &memory.store,
            &memory.wiki,
            &memory.scope,
            &drafts,
            false,
            None,
            now(),
        )
        .expect("first");
        let path = drafts[0].path.clone();
        let page = memory.wiki.locate(&memory.scope.scope, &path);
        std::fs::write(&page, "stale").expect("overwrite");

        let second = seed(
            &memory.store,
            &memory.wiki,
            &memory.scope,
            &drafts,
            true,
            None,
            now(),
        )
        .expect("second");

        assert_eq!(second.written.len(), drafts.len());
        assert!(second.skipped.is_empty());
        assert!(
            std::fs::read_to_string(&page)
                .expect("read")
                .contains("Derived from git history")
        );
    }

    #[test]
    fn ranking_is_stable_across_runs() {
        // Churn and author counts come out of a HashMap, whose iteration
        // order changes between runs. Without a tiebreak, two equal entries
        // would swap places and every re-run would produce a wiki commit that
        // says nothing.
        let fixture = Fixture::new();
        fixture.commit(&[("a.txt", "x"), ("b.txt", "y")], "feat: two", "Ada", at(0));

        let first = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("first");
        let second = survey(fixture.path(), DEFAULT_MAX_COMMITS).expect("second");

        assert_eq!(first.hotspots, second.hotspots);
        assert_eq!(first.authors, second.authors);
    }

    #[test]
    fn root_files_are_counted_not_listed_beside_directories() {
        // A real run listed `.gitignore`, `CHANGELOG.md`, and ten more
        // one-file entries as though they were part of the layout, pushing
        // the directories that actually describe it off the end of the list.
        let files = vec![
            "README.md".to_owned(),
            ".gitignore".to_owned(),
            "src/main.rs".to_owned(),
            "src/lib.rs".to_owned(),
        ];

        let (directories, root) = directory_counts(&files);

        assert_eq!(directories, vec![("src/".to_owned(), 2)]);
        assert_eq!(root, 2);
    }

    #[test]
    fn dotfiles_are_not_mistaken_for_a_language() {
        let files = vec![
            ".gitignore".to_owned(),
            "src/main.rs".to_owned(),
            "src/lib.rs".to_owned(),
        ];

        let counts = extension_counts(&files);

        assert_eq!(counts, vec![("rs".to_owned(), 2)]);
    }
}
