//! Layout of the anamnesis data directory.
//!
//! Memory lives outside the repository it describes:
//!
//! ```text
//! <data_dir>/
//!   wiki/     git-versioned markdown, the source of truth
//!   raw/      immutable sanitized transcripts
//!   db/       SQLite indexes, rebuildable from wiki/
//!   models/   local embedding models
//!   logs/     rolling trace output
//! ```
//!
//! Keeping `wiki/` out of the project repository is what lets it carry its own
//! git history — checkpoints and page restores operate on that history without
//! entangling themselves in the user's commits — and what lets one server hold
//! many projects under one `_global` scope.

use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::scope::Scope;

/// Environment variable that overrides the data directory location.
pub const DATA_DIR_ENV: &str = "ANAMNESIS_DATA_DIR";

/// Directory name used under the platform data directory.
pub const DEFAULT_DIR_NAME: &str = "anamnesis";

/// Filename of the SQLite index inside `db/`.
pub const DB_FILE_NAME: &str = "anamnesis.db";

/// A resolved data directory root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Resolve the data directory.
    ///
    /// Precedence: an explicit path (a CLI flag), then [`DATA_DIR_ENV`], then
    /// the platform data directory. Resolution does not touch the filesystem;
    /// call [`DataDir::ensure_layout`] to create anything.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        if let Some(path) = explicit {
            return Ok(Self::new(path));
        }
        if let Some(value) = std::env::var_os(DATA_DIR_ENV) {
            let path = PathBuf::from(value);
            if path.as_os_str().is_empty() {
                return Err(CoreError::DataDir {
                    reason: format!("{DATA_DIR_ENV} is set but empty"),
                });
            }
            return Ok(Self::new(path));
        }
        let base = dirs::data_dir().ok_or_else(|| CoreError::DataDir {
            reason: format!(
                "no platform data directory is available; set {DATA_DIR_ENV} or pass --data-dir"
            ),
        })?;
        Ok(Self::new(base.join(DEFAULT_DIR_NAME)))
    }

    /// Treat an existing path as the data directory root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root of the data directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Git-versioned wiki root, the source of truth for page content.
    pub fn wiki(&self) -> PathBuf {
        self.root.join("wiki")
    }

    /// Immutable sanitized transcripts.
    pub fn raw(&self) -> PathBuf {
        self.root.join("raw")
    }

    /// Directory holding the SQLite index.
    pub fn db_dir(&self) -> PathBuf {
        self.root.join("db")
    }

    /// Path of the SQLite index itself.
    pub fn db_file(&self) -> PathBuf {
        self.db_dir().join(DB_FILE_NAME)
    }

    /// Directory reserved for local embedding models.
    pub fn models(&self) -> PathBuf {
        self.root.join("models")
    }

    /// Rolling trace output.
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Wiki directory for one scope: `wiki/<workspace>/<project>`.
    ///
    /// Both components are validated names, so no user-supplied string can
    /// escape the wiki root.
    pub fn wiki_scope(&self, scope: &Scope) -> PathBuf {
        self.wiki()
            .join(scope.workspace.as_str())
            .join(scope.project.as_str())
    }

    /// Wiki directory for the cross-project `_global` scope.
    pub fn wiki_global(&self, workspace: &crate::scope::WorkspaceName) -> PathBuf {
        self.wiki().join(workspace.as_str()).join("_global")
    }

    /// Create every directory in the layout, if missing.
    pub fn ensure_layout(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.wiki(),
            self.raw(),
            self.db_dir(),
            self.models(),
            self.logs(),
        ] {
            std::fs::create_dir_all(&dir).map_err(|source| CoreError::io(&dir, source))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{ProjectName, WorkspaceName};

    fn scope(workspace: &str, project: &str) -> Scope {
        Scope {
            workspace: WorkspaceName::parse(workspace).expect("valid workspace"),
            project: ProjectName::parse(project).expect("valid project"),
        }
    }

    #[test]
    fn explicit_path_wins() {
        let dir = DataDir::resolve(Some(PathBuf::from("/srv/memory"))).expect("resolves");
        assert_eq!(dir.root(), Path::new("/srv/memory"));
    }

    #[test]
    fn layout_hangs_off_one_root() {
        let dir = DataDir::new("/srv/memory");
        assert!(dir.wiki().ends_with("wiki"));
        assert!(dir.raw().ends_with("raw"));
        assert!(dir.db_file().ends_with(DB_FILE_NAME));
        assert!(dir.db_file().starts_with(dir.root()));
        assert!(dir.logs().starts_with(dir.root()));
    }

    #[test]
    fn scope_paths_stay_inside_the_wiki_root() {
        let dir = DataDir::new("/srv/memory");
        let path = dir.wiki_scope(&scope("default", "anamnesis"));
        assert!(path.starts_with(dir.wiki()));
        assert!(path.ends_with(Path::new("default").join("anamnesis")));
    }

    #[test]
    fn ensure_layout_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = DataDir::new(tmp.path().join("data"));
        dir.ensure_layout().expect("first run");
        dir.ensure_layout().expect("second run");
        assert!(dir.wiki().is_dir());
        assert!(dir.db_dir().is_dir());
        assert!(dir.models().is_dir());
    }
}
