//! Keeping the index honest about a wiki people edit by hand.
//!
//! The wiki is markdown in a git repository, and the point of that is that you
//! can open it. But the index is what retrieval reads, and until now the only
//! thing that put a hand-written edit into the index was someone remembering to
//! run `anamnesis reindex`. Until they did, search answered from the old text —
//! or kept offering a page that had been deleted.
//!
//! The work is split three ways so that only the last part needs a filesystem:
//!
//! * [`interpret`] turns an absolute path into the page it names, or nothing.
//! * [`sync`] brings one page's index rows in line with the file.
//! * [`run`] is the watcher loop, which is thin on purpose.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anamnesis_core::page::{Page, PagePath};
use anamnesis_core::scope::{ProjectName, Scope, WorkspaceName};
use anamnesis_store::Store;
use anamnesis_wiki::Wiki;
use jiff::Timestamp;
use notify::RecursiveMode;
use notify_debouncer_full::new_debouncer;
use parking_lot::Mutex;

use crate::AppState;

/// How long a file has to stop changing before it is looked at.
///
/// An editor saving a file produces several events, and a synchronising client
/// can produce a burst of them. Waiting is free here — nothing is waiting on
/// the result — and each wasted wake-up costs a page read and a parse.
const SETTLE: Duration = Duration::from_millis(500);

/// A page named by a path under the wiki root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// Workspace and project the page belongs to.
    pub scope: Scope,
    /// Its path within that project.
    pub page: PagePath,
}

/// What syncing one page did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synced {
    /// The file changed, and the index now says what it says.
    Indexed,
    /// The file says what the index already said.
    Unchanged,
    /// The file is gone, and so is its row.
    Forgotten,
    /// The file is gone, and the index never held it.
    Absent,
    /// The file is there but could not be read as a page.
    Unreadable,
}

/// The page an absolute path names, if it names one.
///
/// Everything that is not a page is rejected here rather than deeper in: files
/// outside the root, anything under `.git` (which is the wiki's own history,
/// rewritten constantly by our own commits), anything that is not markdown, and
/// anything whose workspace or project component is not a name this system
/// would ever have written.
pub fn interpret(root: &Path, path: &Path) -> Option<Located> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = relative.components().map(|part| part.as_os_str().to_str());

    let workspace = parts.next()??;
    let project = parts.next()??;
    let rest: Vec<&str> = parts.collect::<Option<Vec<&str>>>()?;
    if rest.is_empty() || rest.contains(&".git") {
        return None;
    }

    let page = rest.join("/");
    if !page.ends_with(".md") {
        return None;
    }

    Some(Located {
        scope: Scope {
            workspace: WorkspaceName::parse(workspace).ok()?,
            // `from_wiki_dir`, not `parse`: the shared `_global` scope is a
            // directory in the wiki like any other, and a page edited there by
            // hand has to reach the index too.
            project: ProjectName::from_wiki_dir(project).ok()?,
        },
        page: PagePath::parse(&page).ok()?,
    })
}

/// Bring one page's index rows in line with the file on disk.
///
/// The comparison in the middle is what makes a watcher safe to point at a
/// directory this system also writes to. Every page consolidation compiles
/// arrives back as a filesystem event, as does every save an editor makes
/// without changing anything, and re-indexing those would move `updated_at` —
/// which a sweep reads as when the page was last written. A wiki that watched
/// itself would renew every page forever and nothing would ever decay.
pub fn sync(
    store: &Store,
    wiki: &Wiki,
    project_id: anamnesis_core::ids::ProjectId,
    located: &Located,
    embedder: Option<&dyn anamnesis_core::embedding::Embed>,
    now: Timestamp,
) -> anyhow::Result<Synced> {
    let file = wiki.scope_root(&located.scope).join(located.page.as_str());
    if !file.exists() {
        let id = anamnesis_core::ids::PageId::derive(project_id, &located.page);
        return Ok(if store.delete_page(id)? {
            Synced::Forgotten
        } else {
            Synced::Absent
        });
    }

    // A page that will not parse is left exactly as the index has it. Someone
    // is mid-edit, or wrote frontmatter that does not hold; either way the last
    // good version is better than nothing, and the next save will be seen too.
    let parsed = match wiki.read_page(&located.scope, &located.page) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::debug!(%error, page = %located.page, "not readable as a page yet");
            return Ok(Synced::Unreadable);
        }
    };

    let body = parsed.body.clone();
    let page = Page::new(
        project_id,
        located.page.clone(),
        parsed.frontmatter,
        parsed.body,
    );

    if store.page_is_current(&page)? {
        return Ok(Synced::Unchanged);
    }

    store.index_page(
        project_id,
        &page,
        &anamnesis_wiki::extract_links(&body),
        embedder,
        now,
    )?;
    Ok(Synced::Indexed)
}

/// Projects this server knows, by the names the wiki is laid out under.
///
/// Rebuilt per batch rather than held: one server serves every project, and a
/// project registered a minute ago should not have to wait for a restart
/// before its wiki is watched.
fn registered(store: &Store) -> HashMap<(String, String), anamnesis_core::ids::ProjectId> {
    store
        .projects()
        .unwrap_or_default()
        .into_iter()
        .map(|project| {
            (
                (
                    project.scope.workspace.as_str().to_owned(),
                    project.scope.project.as_str().to_owned(),
                ),
                project.project_id,
            )
        })
        .collect()
}

/// Apply one batch of changed paths, newest state of each wins.
fn apply(
    store: &Store,
    wiki: &Mutex<Wiki>,
    paths: Vec<PathBuf>,
    root: &Path,
    embedder: Option<&Arc<dyn anamnesis_llm::Embedder>>,
    now: Timestamp,
) {
    let mut located: Vec<Located> = Vec::new();
    for path in paths {
        if let Some(found) = interpret(root, &path)
            && !located.contains(&found)
        {
            located.push(found);
        }
    }
    if located.is_empty() {
        return;
    }

    let projects = registered(store);
    for found in located {
        let key = (
            found.scope.workspace.as_str().to_owned(),
            found.scope.project.as_str().to_owned(),
        );
        // A wiki directory for a project this server has never registered has
        // nothing to attach to. Writing rows for it would invent a project from
        // a directory name, which is exactly what `ProjectKey` exists to stop.
        let Some(project_id) = projects.get(&key).copied() else {
            continue;
        };

        // The lock is taken per page and released between them, the same
        // discipline consolidation follows: one slow page must not hold the
        // wiki against every session ending in the same second.
        let outcome = {
            let wiki = wiki.lock();
            sync(
                store,
                &wiki,
                project_id,
                &found,
                embedder.map(|embedder| embedder.as_ref() as &dyn anamnesis_core::embedding::Embed),
                now,
            )
        };

        match outcome {
            Ok(Synced::Indexed) => {
                tracing::info!(page = %found.page, project = %found.scope, "indexed an edited page")
            }
            Ok(Synced::Forgotten) => {
                tracing::info!(page = %found.page, project = %found.scope, "forgot a deleted page")
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, page = %found.page, "could not sync a page")
            }
        }
    }
}

/// Watch the wiki and keep the index in step with it.
///
/// Blocking: `notify` delivers on its own thread and everything this does —
/// SQLite, git, the wiki mutex — is synchronous, so there is nothing to gain
/// from an async loop and one fewer place for a blocking call to stall the
/// runtime.
pub fn run(state: AppState) {
    let root = state.wiki.lock().root().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer = match new_debouncer(SETTLE, None, tx) {
        Ok(debouncer) => debouncer,
        Err(error) => {
            tracing::warn!(%error, "could not start the wiki watcher; hand edits will need `anamnesis reindex`");
            return;
        }
    };

    if let Err(error) = debouncer.watch(&root, RecursiveMode::Recursive) {
        tracing::warn!(%error, root = %root.display(), "could not watch the wiki; hand edits will need `anamnesis reindex`");
        return;
    }
    tracing::info!(root = %root.display(), "watching the wiki for edits");

    for batch in rx {
        match batch {
            Ok(events) => {
                let paths = events
                    .into_iter()
                    .flat_map(|event| event.paths.clone())
                    .collect();
                apply(
                    &state.store,
                    &state.wiki,
                    paths,
                    &root,
                    state.embedder.as_ref(),
                    Timestamp::now(),
                );
            }
            // Dropped events mean the index may be behind, which is what
            // `reindex` is for. Losing the watcher entirely would be worse.
            Err(errors) => {
                for error in errors {
                    tracing::warn!(%error, "the wiki watcher missed something");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::page::Frontmatter;
    use anamnesis_core::scope::resolve_scope;

    fn root() -> PathBuf {
        PathBuf::from("/data/wiki")
    }

    fn located(path: &str) -> Option<Located> {
        interpret(&root(), &root().join(path))
    }

    #[test]
    fn a_page_under_a_workspace_and_project_is_recognised() {
        let found = located("default/widget/notes/idea.md").expect("a page");
        assert_eq!(found.scope.workspace.as_str(), "default");
        assert_eq!(found.scope.project.as_str(), "widget");
        assert_eq!(found.page.as_str(), "notes/idea.md");
    }

    /// The wiki's own history is rewritten by every commit this system makes,
    /// so watching it would mean re-reading the whole wiki after every write.
    #[test]
    fn the_wikis_git_directory_is_not_a_page() {
        assert_eq!(located("default/widget/.git/objects/ab/cdef.md"), None);
    }

    #[test]
    fn only_markdown_is_a_page() {
        assert_eq!(located("default/widget/notes/diagram.png"), None);
        assert_eq!(located("default/widget/notes/README.txt"), None);
    }

    /// The shared scope is a directory in the wiki like any other. A page
    /// edited there by hand has to reach the index, or `_global/` goes back to
    /// being what it was before anything read it: files nobody sees.
    #[test]
    fn a_page_in_the_shared_scope_is_a_page() {
        let found = located("default/_global/policy/databases.md").expect("a page");
        assert_eq!(found.scope.project.as_str(), "_global");
        assert!(found.scope.project.is_global());
        assert_eq!(found.page.as_str(), "policy/databases.md");
    }

    /// `_global` is the only reserved name that names something. Every other
    /// underscore directory is still not a project.
    #[test]
    fn other_reserved_directories_are_still_not_projects() {
        assert_eq!(located("default/_rules/anything.md"), None);
        assert_eq!(located("default/_slots/anything.md"), None);
    }

    /// The workspace and project directories themselves, and the wiki root, are
    /// not pages — and neither is anything above the root.
    #[test]
    fn paths_that_name_no_page_are_rejected() {
        assert_eq!(located("default"), None);
        assert_eq!(located("default/widget"), None);
        assert_eq!(interpret(&root(), Path::new("/etc/passwd.md")), None);
    }

    struct Harness {
        _repo: tempfile::TempDir,
        _data: tempfile::TempDir,
        store: Store,
        wiki: Wiki,
        scope: anamnesis_core::scope::ResolvedScope,
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
        store.upsert_project(&scope, now()).expect("project");

        Harness {
            wiki: Wiki::open(data.path().join("wiki")).expect("wiki"),
            store,
            scope,
            _repo: repo,
            _data: data,
        }
    }

    fn now() -> Timestamp {
        "2026-08-25T09:00:00Z".parse().expect("timestamp")
    }

    fn write(harness: &Harness, path: &str, title: &str, body: &str) -> Page {
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
        page
    }

    fn located_in(harness: &Harness, path: &str) -> Located {
        Located {
            scope: harness.scope.scope.clone(),
            page: PagePath::parse(path).expect("path"),
        }
    }

    fn sync_one(harness: &Harness, path: &str) -> Synced {
        sync(
            &harness.store,
            &harness.wiki,
            harness.scope.project_id,
            &located_in(harness, path),
            None,
            now(),
        )
        .expect("sync")
    }

    #[test]
    fn a_page_written_by_hand_reaches_the_index() {
        let harness = harness();
        write(&harness, "note.md", "A note", "Body about sqlite.");
        assert_eq!(sync_one(&harness, "note.md"), Synced::Indexed);

        let hits = harness
            .store
            .query_pages(harness.scope.project_id, "sqlite", 10, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1);
    }

    /// The property that makes a watcher safe to point at a directory this
    /// system writes to: seeing our own write again must change nothing.
    #[test]
    fn seeing_the_same_file_again_changes_nothing() {
        let harness = harness();
        write(&harness, "note.md", "A note", "Body.");
        assert_eq!(sync_one(&harness, "note.md"), Synced::Indexed);
        assert_eq!(sync_one(&harness, "note.md"), Synced::Unchanged);
        assert_eq!(sync_one(&harness, "note.md"), Synced::Unchanged);
    }

    #[test]
    fn an_edit_reaches_the_index() {
        let harness = harness();
        write(&harness, "note.md", "A note", "The first thing.");
        sync_one(&harness, "note.md");

        write(&harness, "note.md", "A note", "The second thing.");
        assert_eq!(sync_one(&harness, "note.md"), Synced::Indexed);

        let hits = harness
            .store
            .query_pages(harness.scope.project_id, "second", 10, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn a_page_deleted_by_hand_is_forgotten() {
        let harness = harness();
        write(&harness, "note.md", "A note", "Body.");
        sync_one(&harness, "note.md");

        std::fs::remove_file(
            harness
                .wiki
                .scope_root(&harness.scope.scope)
                .join("note.md"),
        )
        .expect("delete");

        assert_eq!(sync_one(&harness, "note.md"), Synced::Forgotten);
        assert_eq!(sync_one(&harness, "note.md"), Synced::Absent);
        assert!(
            harness
                .store
                .page_paths(harness.scope.project_id)
                .expect("paths")
                .is_empty()
        );
    }

    /// Someone is mid-edit. The last good version is better than none, and the
    /// save that fixes it will be seen too.
    #[test]
    fn a_page_that_will_not_parse_leaves_the_index_alone() {
        let harness = harness();
        write(&harness, "note.md", "A note", "Body about sqlite.");
        sync_one(&harness, "note.md");

        std::fs::write(
            harness
                .wiki
                .scope_root(&harness.scope.scope)
                .join("note.md"),
            "half-typed frontmatter",
        )
        .expect("corrupt");

        assert_eq!(sync_one(&harness, "note.md"), Synced::Unreadable);
        let hits = harness
            .store
            .query_pages(harness.scope.project_id, "sqlite", 10, now(), None)
            .expect("query");
        assert_eq!(hits.len(), 1, "the last good version is still there");
    }
}
