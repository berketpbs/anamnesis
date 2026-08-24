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
use anamnesis_core::scope::Scope;

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

/// A git-backed markdown wiki rooted at one directory.
pub struct Wiki {
    root: PathBuf,
    repo: git2::Repository,
}

impl Wiki {
    /// Open the wiki at `root`, creating the directory and repository if needed.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|source| WikiError::Io {
            path: root.clone(),
            source,
        })?;

        let repo = match git2::Repository::open(&root) {
            Ok(repo) => repo,
            Err(_) => git2::Repository::init(&root)?,
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

    /// Read a page back from disk.
    pub fn read_page(&self, scope: &Scope, path: &PagePath) -> Result<ParsedPage> {
        let absolute = self.locate(scope, path);
        let text = std::fs::read_to_string(&absolute).map_err(|source| WikiError::Io {
            path: absolute,
            source,
        })?;
        parse_document(path.as_str(), &text)
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

    /// Number of commits reachable from HEAD.
    pub fn commit_count(&self) -> Result<usize> {
        let mut walk = self.repo.revwalk()?;
        if walk.push_head().is_err() {
            return Ok(0);
        }
        Ok(walk.count())
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
    fn a_written_page_reads_back_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        let page = sample("We chose SQLite because the index is rebuildable.");

        wiki.write_page(&scope(), &page, "add storage decision").unwrap();
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
            dir.path().join("default").join("anamnesis").join("decisions"),
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
}
