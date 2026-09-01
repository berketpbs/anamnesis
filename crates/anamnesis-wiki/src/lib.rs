//! Git-versioned markdown wiki.
//!
//! The wiki is the source of truth. The SQLite index can be deleted and rebuilt
//! from these files; the reverse is not true. Two properties follow, and both
//! are enforced here rather than left to callers:
//!
//! * **Writes are atomic.** A page is written to a temporary file in its own
//!   directory and renamed into place, so a crash mid-write leaves either the
//!   old page or the new one — never a half-written file that reindexing would
//!   happily parse.
//! * **Every write is a commit.** History is what makes `restore-page` and
//!   checkpoints possible, so committing is not an optional extra step a caller
//!   can forget.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::{Path, PathBuf};

use anamnesis_core::page::{Frontmatter, Page, PagePath};
use anamnesis_core::scope::{ProjectName, ResolvedScope, Scope, WorkspaceName};

mod markdown;

pub use markdown::{ParsedPage, extract_links, parse_document, render_document};

/// Errors produced by the wiki layer.
#[derive(Debug, thiserror::Error)]
pub enum WikiError {
    /// A filesystem operation failed.
    #[error("io error at {path}: {source}")]
    Io {
        /// Path the operation was attempted on.
        path: PathBuf,
        /// Underlying cause.
        #[source]
        source: std::io::Error,
    },

    /// A git operation failed.
    #[error("git error: {0}")]
    Git(Box<git2::Error>),

    /// A page could not be parsed.
    #[error("malformed page at {path}: {reason}")]
    Malformed {
        /// Page that failed to parse.
        path: String,
        /// What was wrong with it.
        reason: String,
    },

    /// A core validation rejected the input.
    #[error(transparent)]
    Core(#[from] anamnesis_core::CoreError),
}

impl From<git2::Error> for WikiError {
    fn from(source: git2::Error) -> Self {
        Self::Git(Box::new(source))
    }
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, WikiError>;

/// Identity recorded on wiki commits.
const COMMIT_NAME: &str = "anamnesis";
/// Address recorded on wiki commits.
const COMMIT_EMAIL: &str = "anamnesis@localhost";
/// Branch a newly created wiki starts on.
///
/// Named explicitly because `libgit2` still defaults to `master` and ignores
/// `init.defaultBranch` entirely — so without this the branch a wiki lives on
/// depends on which library created it, and someone pushing their memory to a
/// remote gets a branch name they never chose.
const INITIAL_BRANCH: &str = "main";

/// A git-backed markdown wiki rooted at one directory.
pub struct Wiki {
    root: PathBuf,
    repo: git2::Repository,
}

impl Wiki {
    /// Open the wiki at `root`, creating the directory and repository if needed.
    ///
    /// A repository that already exists is opened as it is, whatever branch it
    /// is on: renaming someone's branch out from under them — possibly one
    /// they have already pushed — is not something opening a wiki should do.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| WikiError::Io {
            path: root.clone(),
            source,
        })?;

        let repo = match git2::Repository::open(&root) {
            Ok(repo) => repo,
            Err(_) => {
                let mut options = git2::RepositoryInitOptions::new();
                options.initial_head(INITIAL_BRANCH);
                git2::Repository::init_opts(&root, &options)?
            }
        };

        Ok(Self { root, repo })
    }

    /// Root directory of the wiki.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute location of a page.
    pub fn locate(&self, scope: &Scope, path: &PagePath) -> PathBuf {
        self.root
            .join(scope.workspace.as_str())
            .join(scope.project.as_str())
            .join(path.as_str())
    }

    /// Whether a page exists on disk.
    pub fn exists(&self, scope: &Scope, path: &PagePath) -> bool {
        self.locate(scope, path).is_file()
    }

    /// Write a page and commit it, returning the commit id.
    pub fn write_page(&self, scope: &Scope, page: &Page, message: &str) -> Result<String> {
        let absolute = self.locate(scope, &page.path);
        let parent = absolute
            .parent()
            .expect("a page path always has a parent directory")
            .to_path_buf();
        std::fs::create_dir_all(&parent).map_err(|source| WikiError::Io {
            path: parent.clone(),
            source,
        })?;

        let document = render_document(&page.frontmatter, &page.body)?;
        write_atomically(&absolute, document.as_bytes())?;

        let relative = self.relative(scope, &page.path);
        self.commit(&relative, message)
    }

    /// Delete pages and record their removal in one commit.
    ///
    /// One commit, not one per page: a sweep is a single decision about a
    /// project's memory, and reading its history as forty commits that each
    /// drop one file hides the shape of what happened. The message is where
    /// the individual pages belong.
    ///
    /// Nothing is lost by this in the sense that matters — the wiki is a git
    /// repository, and every deleted page stays reachable in the history that
    /// records its removal.
    ///
    /// Returns the commit id, or `None` when there was nothing for git to
    /// record: an empty list, or pages that were only ever on disk and never
    /// committed. A missing file is not an error — a sweep whose page someone
    /// already deleted by hand has arrived at the state it wanted.
    pub fn delete_pages(
        &self,
        scope: &Scope,
        paths: &[PagePath],
        message: &str,
    ) -> Result<Option<String>> {
        if paths.is_empty() {
            return Ok(None);
        }

        let scope_root = self
            .root
            .join(scope.workspace.as_str())
            .join(scope.project.as_str());

        for path in paths {
            let absolute = self.locate(scope, path);
            match std::fs::remove_file(&absolute) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(WikiError::Io {
                        path: absolute,
                        source,
                    });
                }
            }
            prune_empty_parents(&absolute, &scope_root);
        }

        let relatives: Vec<PathBuf> = paths
            .iter()
            .map(|path| self.relative(scope, path))
            .collect();
        self.commit_removals(&relatives, message)
    }

    /// Move every page from one scope's directory to another's, in one commit.
    ///
    /// A rename, and git is told it is one: the removals and the additions are
    /// staged together, so the history shows a move rather than a deletion
    /// followed by an unrelated arrival. That matters because the wiki's
    /// history is the only thing standing behind a page after it is gone from
    /// HEAD, and `git log --follow` is how somebody finds it.
    ///
    /// The pages themselves are byte-for-byte what they were. Only the
    /// directory they live in changes — which is exactly what a project being
    /// renamed means on disk.
    pub fn move_scope(&self, from: &Scope, to: &Scope, message: &str) -> Result<Option<String>> {
        let source = self.scope_root(from);
        let destination = self.scope_root(to);
        if !source.exists() {
            return Ok(None);
        }
        if destination.exists() {
            return Err(WikiError::Io {
                path: destination,
                source: std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "the destination scope already has a directory",
                ),
            });
        }

        let paths = self.pages(from)?;
        let removals: Vec<PathBuf> = paths.iter().map(|path| self.relative(from, path)).collect();

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|source| WikiError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::rename(&source, &destination).map_err(|source_error| WikiError::Io {
            path: source.clone(),
            source: source_error,
        })?;

        let mut index = self.repo.index()?;
        for relative in &removals {
            let _ = index.remove_path(relative);
        }
        for path in &paths {
            index.add_path(&self.relative(to, path))?;
        }
        index.write()?;
        let tree_id = index.write_tree()?;

        let parents = match self.repo.head() {
            Ok(head) => vec![head.peel_to_commit()?],
            Err(_) => Vec::new(),
        };
        // Nothing moved that git was tracking — a scope whose pages were never
        // committed. The files are where they should be either way, and an
        // empty commit would claim otherwise.
        if let Some(parent) = parents.first()
            && parent.tree_id() == tree_id
        {
            return Ok(None);
        }

        let tree = self.repo.find_tree(tree_id)?;
        let signature = git2::Signature::now(COMMIT_NAME, COMMIT_EMAIL)?;
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        let id = self.repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )?;
        Ok(Some(id.to_string()))
    }

    /// Read a page back from disk.
    pub fn read_page(&self, scope: &Scope, path: &PagePath) -> Result<ParsedPage> {
        let absolute = self.locate(scope, path);
        let text = std::fs::read_to_string(&absolute).map_err(|source| WikiError::Io {
            path: absolute,
            source,
        })?;
        parse_document(path.as_str(), &text)
    }

    /// Every page in a scope, in sorted path order.
    ///
    /// Walks the filesystem rather than asking git or the index: a rebuild
    /// runs precisely when the index is gone, and a page someone dropped in
    /// by hand — with Obsidian, or a text editor — is as real as one this
    /// crate wrote. A file whose name is not a valid [`PagePath`] is skipped
    /// rather than failing the walk, since the wiki directory belongs to the
    /// user and may hold anything.
    pub fn pages(&self, scope: &Scope) -> Result<Vec<PagePath>> {
        let root = self.scope_root(scope);
        let mut found = Vec::new();
        collect_pages(&root, &root, &mut found)?;
        found.sort();
        Ok(found)
    }

    /// The workspace's shared `_global` scope, as this wiki lays it out.
    ///
    /// Every reader of the shared scope has to derive the same two things —
    /// where its pages sit, and the project identifier its index rows carry —
    /// and they have to agree, or a page written through one is invisible to
    /// the other. Derived once, here, because the wiki is what decides where
    /// a scope's pages live.
    pub fn global_scope(&self, workspace: &WorkspaceName) -> ResolvedScope {
        let scope = Scope {
            workspace: workspace.clone(),
            project: ProjectName::global(),
        };
        ResolvedScope::global(workspace, self.scope_root(&scope))
    }

    /// Directory holding one scope's pages, whether or not it exists.
    ///
    /// Exposed because "the wiki has no pages" and "the wiki is not there" are
    /// the same empty list to [`Wiki::pages`], and a caller deciding what to do
    /// about pages it cannot find needs to tell those apart.
    pub fn scope_root(&self, scope: &Scope) -> PathBuf {
        self.root
            .join(scope.workspace.as_str())
            .join(scope.project.as_str())
    }

    /// Path of a page relative to the wiki root, in git's forward-slash form.
    fn relative(&self, scope: &Scope, path: &PagePath) -> PathBuf {
        PathBuf::from(scope.workspace.as_str())
            .join(scope.project.as_str())
            .join(path.as_str())
    }

    /// Stage one path and commit it onto HEAD.
    fn commit(&self, relative: &Path, message: &str) -> Result<String> {
        let mut index = self.repo.index()?;
        index.add_path(relative)?;
        index.write()?;
        let tree = self.repo.find_tree(index.write_tree()?)?;

        let signature = git2::Signature::now(COMMIT_NAME, COMMIT_EMAIL)?;
        let parents = match self.repo.head() {
            Ok(head) => vec![head.peel_to_commit()?],
            // No HEAD yet: this is the first commit in a fresh repository.
            Err(_) => Vec::new(),
        };
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

        let id = self.repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )?;
        Ok(id.to_string())
    }

    /// Stage a set of removals and commit them together.
    ///
    /// A path git never tracked produces no error: the file is gone either
    /// way, and the caller asked for a state, not for a transaction. When
    /// that leaves the tree exactly as HEAD already had it, no commit is
    /// made — an empty commit would claim a sweep changed something it did
    /// not.
    fn commit_removals(&self, relatives: &[PathBuf], message: &str) -> Result<Option<String>> {
        let mut index = self.repo.index()?;
        for relative in relatives {
            let _ = index.remove_path(relative);
        }
        index.write()?;
        let tree_id = index.write_tree()?;

        let parents = match self.repo.head() {
            Ok(head) => vec![head.peel_to_commit()?],
            Err(_) => Vec::new(),
        };
        if let Some(parent) = parents.first()
            && parent.tree_id() == tree_id
        {
            return Ok(None);
        }

        let tree = self.repo.find_tree(tree_id)?;
        let signature = git2::Signature::now(COMMIT_NAME, COMMIT_EMAIL)?;
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        let id = self.repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )?;
        Ok(Some(id.to_string()))
    }

    /// Number of commits reachable from HEAD.
    pub fn commit_count(&self) -> Result<usize> {
        let mut walk = self.repo.revwalk()?;
        if walk.push_head().is_err() {
            return Ok(0);
        }
        Ok(walk.count())
    }
}

/// Remove the directories a deleted page left empty, up to `stop`.
///
/// Git does not track directories, so this changes nothing in the history —
/// it keeps the working copy honest, so that someone browsing the wiki in a
/// file manager or in Obsidian does not find a tree of empty folders naming
/// subjects the memory no longer holds. Failures are ignored on purpose: a
/// directory that will not go (a stray file, a permission) is not a reason to
/// fail a deletion that already succeeded.
fn prune_empty_parents(deleted: &Path, stop: &Path) {
    let mut current = deleted.parent();
    while let Some(dir) = current {
        if dir == stop || !dir.starts_with(stop) || std::fs::remove_dir(dir).is_err() {
            return;
        }
        current = dir.parent();
    }
}

/// Build a page from its parts and hand it to the wiki.
///
/// Exists so callers do not have to remember that the page identifier is
/// derived rather than chosen.
pub fn page(
    project_id: anamnesis_core::ids::ProjectId,
    path: PagePath,
    frontmatter: Frontmatter,
    body: impl Into<String>,
) -> Page {
    Page::new(project_id, path, frontmatter, body)
}

/// Walk `dir` collecting markdown files as paths relative to `root`.
///
/// A missing directory is an empty scope, not an error: a project that has
/// never had a page written is the state every new project starts in.
fn collect_pages(root: &Path, dir: &Path, found: &mut Vec<PagePath>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(WikiError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|source| WikiError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();

        if path.is_dir() {
            // `.git` holds the wiki's own history, not pages.
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            collect_pages(root, &path, found)?;
        } else if path.extension().is_some_and(|ext| ext == "md")
            && let Ok(relative) = path.strip_prefix(root)
            && let Some(text) = relative.to_str()
            && let Ok(page) = PagePath::parse(&text.replace('\\', "/"))
        {
            found.push(page);
        }
    }
    Ok(())
}

/// Write bytes to `target` without ever leaving a partial file in place.
fn write_atomically(target: &Path, contents: &[u8]) -> Result<()> {
    let directory = target
        .parent()
        .expect("target always has a parent directory");
    // The temporary file must share a directory with the target: rename is only
    // atomic within one filesystem.
    let temporary = directory.join(format!(
        ".{}.tmp",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "page".to_owned())
    ));

    std::fs::write(&temporary, contents).map_err(|source| WikiError::Io {
        path: temporary.clone(),
        source,
    })?;

    // Windows refuses to rename onto an existing file, so clear the way first.
    // The window this opens is why the temporary file is kept, not discarded.
    if target.exists() {
        std::fs::remove_file(target).map_err(|source| WikiError::Io {
            path: target.to_path_buf(),
            source,
        })?;
    }

    std::fs::rename(&temporary, target).map_err(|source| WikiError::Io {
        path: target.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::ids::ProjectId;
    use anamnesis_core::page::{PageStatus, Tier};
    use anamnesis_core::scope::{ProjectName, WorkspaceName};

    fn scope() -> Scope {
        Scope {
            workspace: WorkspaceName::parse("default").unwrap(),
            project: ProjectName::parse("anamnesis").unwrap(),
        }
    }

    fn sample(body: &str) -> Page {
        let mut frontmatter = Frontmatter::new("Storage engine", Vec::new()).unwrap();
        frontmatter.tier = Tier::Semantic;
        Page::new(
            ProjectId::from_uuid(uuid::Uuid::nil()),
            PagePath::parse("decisions/0001-storage.md").unwrap(),
            frontmatter,
            body,
        )
    }

    #[test]
    fn a_new_wiki_starts_on_main() {
        // libgit2 ignores `init.defaultBranch` and would pick `master`, so the
        // branch a wiki lives on would otherwise depend on which library
        // happened to create it.
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        wiki.write_page(&scope(), &sample("body"), "first").unwrap();

        let head = wiki.repo.head().unwrap();
        assert_eq!(head.shorthand().unwrap(), "main");
    }

    #[test]
    fn an_existing_wiki_keeps_the_branch_it_is_on() {
        // Reopening must not rename a branch someone may already have pushed.
        let dir = tempfile::tempdir().unwrap();
        let mut options = git2::RepositoryInitOptions::new();
        options.initial_head("master");
        git2::Repository::init_opts(dir.path(), &options).unwrap();

        let wiki = Wiki::open(dir.path()).unwrap();
        wiki.write_page(&scope(), &sample("body"), "first").unwrap();

        let head = wiki.repo.head().unwrap();
        assert_eq!(head.shorthand().unwrap(), "master");
    }

    #[test]
    fn a_written_page_reads_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = sample("We chose SQLite because the index is rebuildable.");

        wiki.write_page(&scope(), &page, "add storage decision")
            .unwrap();
        let read = wiki.read_page(&scope(), &page.path).unwrap();

        assert_eq!(read.frontmatter.title, "Storage engine");
        assert_eq!(read.frontmatter.tier, Tier::Semantic);
        assert_eq!(read.frontmatter.status, PageStatus::Active);
        assert_eq!(read.body.trim(), page.body);
    }

    #[test]
    fn writing_creates_a_commit_per_write() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        assert_eq!(wiki.commit_count().unwrap(), 0);

        let mut page = sample("first");
        wiki.write_page(&scope(), &page, "first").unwrap();
        assert_eq!(wiki.commit_count().unwrap(), 1);

        page.body = "second".to_owned();
        wiki.write_page(&scope(), &page, "second").unwrap();
        assert_eq!(wiki.commit_count().unwrap(), 2);
    }

    #[test]
    fn rewriting_a_page_replaces_it_rather_than_appending() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();

        let mut page = sample("original body");
        wiki.write_page(&scope(), &page, "first").unwrap();
        page.body = "revised body".to_owned();
        wiki.write_page(&scope(), &page, "second").unwrap();

        let read = wiki.read_page(&scope(), &page.path).unwrap();
        assert_eq!(read.body.trim(), "revised body");
        assert!(!read.body.contains("original"));
    }

    #[test]
    fn pages_land_under_their_scope() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = sample("body");
        wiki.write_page(&scope(), &page, "write").unwrap();

        let expected = dir
            .path()
            .join("default")
            .join("anamnesis")
            .join("decisions")
            .join("0001-storage.md");
        assert!(expected.is_file(), "page not at {}", expected.display());
    }

    #[test]
    fn no_temporary_files_survive_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = sample("body");
        wiki.write_page(&scope(), &page, "write").unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(
            dir.path()
                .join("default")
                .join("anamnesis")
                .join("decisions"),
        )
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
        assert!(leftovers.is_empty(), "temporary file left behind");
    }

    #[test]
    fn reopening_finds_the_existing_repository() {
        let dir = tempfile::tempdir().unwrap();
        {
            let wiki = Wiki::open(dir.path()).unwrap();
            wiki.write_page(&scope(), &sample("body"), "write").unwrap();
        }
        let wiki = Wiki::open(dir.path()).unwrap();
        assert_eq!(wiki.commit_count().unwrap(), 1, "history should survive");
    }

    #[test]
    fn listing_finds_every_page_in_a_scope_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();

        for path in ["notes/b.md", "decisions/0001-storage.md", "top.md"] {
            let mut page = sample("body");
            page.path = PagePath::parse(path).unwrap();
            wiki.write_page(&scope(), &page, "write").unwrap();
        }

        let listed: Vec<String> = wiki
            .pages(&scope())
            .unwrap()
            .iter()
            .map(|p| p.as_str().to_owned())
            .collect();
        assert_eq!(
            listed,
            vec![
                "decisions/0001-storage.md".to_owned(),
                "notes/b.md".to_owned(),
                "top.md".to_owned(),
            ]
        );
    }

    #[test]
    fn listing_a_scope_that_has_no_pages_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        assert!(wiki.pages(&scope()).unwrap().is_empty());
    }

    #[test]
    fn listing_ignores_git_internals_and_non_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        wiki.write_page(&scope(), &sample("body"), "write").unwrap();

        // A page written by hand, and two things that are not pages.
        let project = dir.path().join("default").join("anamnesis");
        std::fs::write(
            project.join("by-hand.md"),
            "---\ntitle: Hand\n---\n\nbody\n",
        )
        .unwrap();
        std::fs::write(project.join("notes.txt"), "not a page").unwrap();
        std::fs::create_dir_all(project.join(".git").join("refs")).unwrap();
        std::fs::write(project.join(".git").join("refs").join("x.md"), "not a page").unwrap();

        let listed: Vec<String> = wiki
            .pages(&scope())
            .unwrap()
            .iter()
            .map(|p| p.as_str().to_owned())
            .collect();
        assert_eq!(
            listed,
            vec![
                "by-hand.md".to_owned(),
                "decisions/0001-storage.md".to_owned()
            ],
            "a hand-written page counts; .git and non-markdown do not"
        );
    }
    /// A second page, so a sweep has something to leave behind.
    fn other(path: &str) -> Page {
        let mut frontmatter = Frontmatter::new("Session notes", Vec::new()).unwrap();
        frontmatter.tier = Tier::Episodic;
        Page::new(
            ProjectId::from_uuid(uuid::Uuid::nil()),
            PagePath::parse(path).unwrap(),
            frontmatter,
            "what happened",
        )
    }

    #[test]
    fn deleting_a_page_removes_the_file_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = sample("body");
        wiki.write_page(&scope(), &page, "write").unwrap();
        let before = wiki.commit_count().unwrap();

        let commit = wiki
            .delete_pages(
                &scope(),
                std::slice::from_ref(&page.path),
                "sweep: forget 1 page",
            )
            .unwrap()
            .expect("a commit");

        assert!(!wiki.exists(&scope(), &page.path));
        assert!(wiki.pages(&scope()).unwrap().is_empty());
        assert_eq!(wiki.commit_count().unwrap(), before + 1);
        assert!(!commit.is_empty());
    }

    #[test]
    fn a_deleted_page_is_still_in_the_history() {
        // The whole reason a sweep may delete at all: the wiki is a git
        // repository, so forgetting is reversible by anyone who needs it.
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = sample("the body that must survive deletion");
        wiki.write_page(&scope(), &page, "write").unwrap();
        wiki.delete_pages(&scope(), std::slice::from_ref(&page.path), "sweep")
            .unwrap();

        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let previous = head.parent(0).unwrap();
        let blob = previous
            .tree()
            .unwrap()
            .get_path(Path::new("default/anamnesis/decisions/0001-storage.md"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        let text = std::str::from_utf8(blob.as_blob().unwrap().content()).unwrap();
        assert!(text.contains("the body that must survive deletion"));
    }

    #[test]
    fn many_pages_leave_in_one_commit() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let pages = ["sessions/a.md", "sessions/b.md", "sessions/c.md"].map(other);
        for page in &pages {
            wiki.write_page(&scope(), page, "write").unwrap();
        }
        let before = wiki.commit_count().unwrap();

        let paths: Vec<PagePath> = pages.iter().map(|page| page.path.clone()).collect();
        wiki.delete_pages(&scope(), &paths, "sweep: forget 3 pages")
            .unwrap()
            .expect("a commit");

        assert_eq!(wiki.commit_count().unwrap(), before + 1);
        assert!(wiki.pages(&scope()).unwrap().is_empty());
    }

    #[test]
    fn deleting_leaves_the_pages_it_was_not_asked_about() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let doomed = other("sessions/old.md");
        let kept = other("sessions/new.md");
        wiki.write_page(&scope(), &doomed, "write").unwrap();
        wiki.write_page(&scope(), &kept, "write").unwrap();

        wiki.delete_pages(&scope(), std::slice::from_ref(&doomed.path), "sweep")
            .unwrap();

        assert_eq!(
            wiki.pages(&scope()).unwrap(),
            vec![kept.path.clone()],
            "only the page named was forgotten"
        );
    }

    #[test]
    fn a_file_someone_already_deleted_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = sample("body");
        wiki.write_page(&scope(), &page, "write").unwrap();
        std::fs::remove_file(wiki.locate(&scope(), &page.path)).unwrap();

        // Still committed, because git still had the file staged: the sweep
        // is what records the removal.
        assert!(
            wiki.delete_pages(&scope(), std::slice::from_ref(&page.path), "sweep")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn deleting_nothing_makes_no_commit() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        wiki.write_page(&scope(), &sample("body"), "write").unwrap();
        let before = wiki.commit_count().unwrap();

        assert!(wiki.delete_pages(&scope(), &[], "sweep").unwrap().is_none());
        assert_eq!(wiki.commit_count().unwrap(), before);
    }

    #[test]
    fn deleting_a_page_git_never_saw_makes_no_commit() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        wiki.write_page(&scope(), &sample("body"), "write").unwrap();
        let before = wiki.commit_count().unwrap();

        // Dropped in by hand and never committed: there is nothing for git to
        // record, and an empty commit would say a sweep happened.
        let project = dir.path().join("default").join("anamnesis");
        std::fs::write(
            project.join("by-hand.md"),
            "---
title: Hand
---

body
",
        )
        .unwrap();
        let by_hand = PagePath::parse("by-hand.md").unwrap();

        assert!(
            wiki.delete_pages(&scope(), std::slice::from_ref(&by_hand), "sweep")
                .unwrap()
                .is_none()
        );
        assert!(!wiki.exists(&scope(), &by_hand), "the file is still gone");
        assert_eq!(wiki.commit_count().unwrap(), before);
    }

    #[test]
    fn emptied_directories_do_not_linger() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = other("sessions/2026/08/only.md");
        wiki.write_page(&scope(), &page, "write").unwrap();

        wiki.delete_pages(&scope(), std::slice::from_ref(&page.path), "sweep")
            .unwrap();

        let project = dir.path().join("default").join("anamnesis");
        assert!(!project.join("sessions").exists());
        assert!(project.exists(), "the project directory itself stays");
    }

    #[test]
    fn a_directory_with_pages_left_in_it_survives() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let doomed = other("sessions/old.md");
        let kept = other("sessions/new.md");
        wiki.write_page(&scope(), &doomed, "write").unwrap();
        wiki.write_page(&scope(), &kept, "write").unwrap();

        wiki.delete_pages(&scope(), std::slice::from_ref(&doomed.path), "sweep")
            .unwrap();

        assert!(
            dir.path()
                .join("default")
                .join("anamnesis")
                .join("sessions")
                .exists()
        );
    }
}
