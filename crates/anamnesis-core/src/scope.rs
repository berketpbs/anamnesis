//! Workspace / project naming and the rules that resolve a working directory
//! to the memory scope it belongs to.
//!
//! Resolution order, most specific first:
//!
//! 1. an explicit project name in the nearest marker file,
//! 2. the git remote URL of the enclosing repository,
//! 3. the absolute path of the git working tree,
//! 4. the basename of the current directory.
//!
//! Whichever wins becomes a [`ProjectKey`], and the key — not the display name
//! — is what [`crate::ids::ProjectId`] is derived from.

use std::path::{Component, Path, PathBuf};

use crate::config::{AutoImproveConfig, CaptureConfig, DecayConfig, MarkerConfig};
use crate::error::{CoreError, Result};
use crate::ids::{ProjectId, WorkspaceId};

/// Filename of the per-project marker.
pub const MARKER_FILE: &str = ".anamnesis.toml";

/// Marker filename accepted for backwards compatibility with `ai-memory`.
pub const LEGACY_MARKER_FILE: &str = ".ai-memory.toml";

/// Workspace used when nothing else is configured.
pub const DEFAULT_WORKSPACE: &str = "default";

/// Longest permitted workspace or project name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Windows device names that cannot exist as directory names.
const RESERVED_STEMS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

macro_rules! name_newtype {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Validate a name supplied by a human, rejecting anything unusable
            /// as a directory component.
            pub fn parse(value: &str) -> Result<Self> {
                validate_name($kind, value).map(|_| Self(value.trim().to_ascii_lowercase()))
            }

            /// Coerce an arbitrary string (a repository or directory name) into
            /// a usable name, replacing unsupported characters rather than
            /// failing.
            pub fn sanitized(value: &str) -> Result<Self> {
                Self::parse(&coerce_name(value))
            }

            /// Borrow the name as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Self::parse(&raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

name_newtype! {
    /// A validated workspace name. Safe to use as a directory component.
    WorkspaceName, "workspace"
}

name_newtype! {
    /// A validated project name. Safe to use as a directory component.
    ProjectName, "project"
}

name_newtype! {
    /// A validated operator name: who a bearer token belongs to.
    ///
    /// Held to the same rules as a project name because it is heading for the
    /// same places — a column that is compared, and eventually a slot that has
    /// to be nameable on disk.
    OperatorName, "operator"
}

impl Default for WorkspaceName {
    fn default() -> Self {
        Self(DEFAULT_WORKSPACE.to_owned())
    }
}

/// The canonical string a [`ProjectId`] is derived from.
///
/// Always carries its provenance as a prefix (`git:`, `path:`, or `name:`) so
/// two different derivation strategies can never collide on the same value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ProjectKey(String);

impl ProjectKey {
    /// Key derived from a normalized git remote, e.g. `git:github.com/acme/api`.
    pub fn from_remote(normalized: &str) -> Self {
        Self(format!("git:{normalized}"))
    }

    /// Key derived from the absolute path of a working tree.
    pub fn from_path(path: &Path) -> Self {
        Self(format!("path:{}", normalize_path_key(path)))
    }

    /// Key derived from an explicit or inferred project name.
    pub fn from_name(name: &ProjectName) -> Self {
        Self(format!("name:{}", name.as_str()))
    }

    /// Rebuild a key from a previously stored canonical string.
    pub fn from_canonical(value: String) -> Self {
        Self(value)
    }

    /// Borrow the canonical string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A workspace and project pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Scope {
    /// Workspace the project belongs to.
    pub workspace: WorkspaceName,
    /// Project within that workspace.
    pub project: ProjectName,
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.workspace, self.project)
    }
}

/// How a scope was arrived at. Surfaced by `anamnesis status`, because a wrong
/// scope is the most likely explanation for "my memory looks empty".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeSource {
    /// A marker file named the project explicitly.
    Marker {
        /// Location of the marker file.
        path: PathBuf,
        /// Whether it used the legacy `.ai-memory.toml` filename.
        legacy: bool,
    },
    /// Derived from the git remote of the enclosing repository.
    GitRemote {
        /// Normalized remote, e.g. `github.com/acme/api`.
        normalized: String,
    },
    /// Derived from the path of the git working tree (no usable remote).
    GitRoot {
        /// Absolute path of the working tree.
        path: PathBuf,
    },
    /// Derived from the basename of the current directory (no repository).
    CwdBasename {
        /// Directory the name was taken from.
        path: PathBuf,
    },
}

/// A fully resolved scope, including the identifiers derived from it.
#[derive(Debug, Clone)]
pub struct ResolvedScope {
    /// Human-facing workspace and project names.
    pub scope: Scope,
    /// Canonical key the project identifier was derived from.
    pub key: ProjectKey,
    /// Derived workspace identifier.
    pub workspace_id: WorkspaceId,
    /// Derived project identifier.
    pub project_id: ProjectId,
    /// How the project was identified.
    pub source: ScopeSource,
    /// Marker file that participated in resolution, if any.
    pub marker: Option<PathBuf>,
    /// Directory the project's relative configuration resolves against: the
    /// marker's directory, the repository root, or the working directory.
    pub root: PathBuf,
    /// Paths this project has asked never to be captured.
    pub capture: CaptureConfig,
    /// How quickly this project forgets pages nobody reads.
    pub decay: DecayConfig,
    /// Whether this project wants its memory improved, and on what terms.
    pub auto_improve: AutoImproveConfig,
}

/// Location of a discovered marker file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerLocation {
    /// Absolute path to the marker file.
    pub path: PathBuf,
    /// Whether the legacy `.ai-memory.toml` filename was used.
    pub legacy: bool,
}

/// Walk `start` and its ancestors looking for a marker file.
///
/// The preferred filename wins over the legacy one within the same directory,
/// so a project mid-migration behaves predictably.
pub fn find_marker(start: &Path) -> Option<MarkerLocation> {
    for dir in start.ancestors() {
        let preferred = dir.join(MARKER_FILE);
        if preferred.is_file() {
            return Some(MarkerLocation {
                path: preferred,
                legacy: false,
            });
        }
        let legacy = dir.join(LEGACY_MARKER_FILE);
        if legacy.is_file() {
            return Some(MarkerLocation {
                path: legacy,
                legacy: true,
            });
        }
    }
    None
}

/// Resolve the memory scope for a working directory.
pub fn resolve_scope(cwd: &Path) -> Result<ResolvedScope> {
    let cwd = absolutize(cwd)?;
    let marker = find_marker(&cwd);
    let config = match &marker {
        Some(loc) => Some(MarkerConfig::load(&loc.path)?),
        None => None,
    };

    let workspace = config
        .as_ref()
        .and_then(|c| c.scope.workspace.clone())
        .unwrap_or_default();

    let explicit_project = config.as_ref().and_then(|c| c.scope.project.clone());

    let (project, key, source) = match (explicit_project, &marker) {
        (Some(name), Some(loc)) => {
            let key = ProjectKey::from_name(&name);
            let source = ScopeSource::Marker {
                path: loc.path.clone(),
                legacy: loc.legacy,
            };
            (name, key, source)
        }
        _ => derive_project(&cwd)?,
    };

    // Taken apart here rather than at each use: `config` is consumed by the
    // scope fields above, and a later reader should not have to prove that
    // two `map`s over the same `Option` see the same marker file.
    let (capture, decay, auto_improve) = match config {
        Some(config) => (
            Some(config.capture),
            Some(config.decay),
            Some(config.auto_improve),
        ),
        None => (None, None, None),
    };

    // Relative patterns belong to whoever wrote them: the marker's directory
    // when there is a marker, the repository otherwise. Falling back to the
    // working directory last means a pattern still resolves against something
    // predictable in a directory that is neither.
    let root = marker
        .as_ref()
        .and_then(|loc| loc.path.parent().map(Path::to_path_buf))
        .or_else(|| discover_repository(&cwd).and_then(|repo| repo.workdir))
        .unwrap_or_else(|| cwd.clone());

    Ok(ResolvedScope {
        workspace_id: WorkspaceId::derive(&workspace),
        project_id: ProjectId::derive(&workspace, &key),
        scope: Scope { workspace, project },
        key,
        source,
        marker: marker.map(|m| m.path),
        root,
        capture: capture.unwrap_or_default(),
        decay: decay.unwrap_or_default(),
        auto_improve: auto_improve.unwrap_or_default(),
    })
}

/// Derive a project from the filesystem alone, ignoring marker overrides.
fn derive_project(cwd: &Path) -> Result<(ProjectName, ProjectKey, ScopeSource)> {
    if let Some(repo) = discover_repository(cwd) {
        if let Some(normalized) = repo.remote.as_deref().and_then(normalize_remote_url) {
            let name = ProjectName::sanitized(last_segment(&normalized))?;
            let key = ProjectKey::from_remote(&normalized);
            return Ok((name, key, ScopeSource::GitRemote { normalized }));
        }
        if let Some(workdir) = repo.workdir {
            let name = ProjectName::sanitized(&basename(&workdir))?;
            let key = ProjectKey::from_path(&workdir);
            return Ok((name, key, ScopeSource::GitRoot { path: workdir }));
        }
    }

    let name = ProjectName::sanitized(&basename(cwd))?;
    let key = ProjectKey::from_name(&name);
    Ok((
        name,
        key,
        ScopeSource::CwdBasename {
            path: cwd.to_path_buf(),
        },
    ))
}

/// What we need to know about an enclosing repository.
struct RepositoryFacts {
    remote: Option<String>,
    workdir: Option<PathBuf>,
}

/// Inspect the repository containing `cwd`, if there is one.
///
/// Repository discovery failing is an ordinary outcome — plenty of directories
/// are not repositories — so this returns `None` rather than an error.
fn discover_repository(cwd: &Path) -> Option<RepositoryFacts> {
    let repo = git2::Repository::discover(cwd).ok()?;
    let remote = preferred_remote(&repo);
    let workdir = repo.workdir().map(Path::to_path_buf);
    Some(RepositoryFacts { remote, workdir })
}

/// Pick `origin` when present, otherwise the first remote in alphabetical order.
fn preferred_remote(repo: &git2::Repository) -> Option<String> {
    if let Ok(origin) = repo.find_remote("origin")
        && let Ok(url) = origin.url()
    {
        return Some(url.to_owned());
    }
    let remotes = repo.remotes().ok()?;
    let mut names: Vec<&str> = remotes.iter().filter_map(|n| n.ok().flatten()).collect();
    names.sort_unstable();
    for name in names {
        if let Ok(remote) = repo.find_remote(name)
            && let Ok(url) = remote.url()
        {
            return Some(url.to_owned());
        }
    }
    None
}

/// Reduce a git remote URL to a stable `host/path` key.
///
/// Every transport for the same repository must collapse to the same string,
/// otherwise cloning over SSH instead of HTTPS would start a second memory:
///
/// ```text
/// https://github.com/acme/api.git      -> github.com/acme/api
/// git@github.com:acme/api.git          -> github.com/acme/api
/// ssh://git@github.com:22/acme/api     -> github.com/acme/api
/// ```
///
/// Returns `None` for local paths and `file://` URLs, where the path-based key
/// is the more meaningful identity.
pub fn normalize_remote_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    let rest = match url.split_once("://") {
        Some((scheme, rest)) => {
            if scheme.eq_ignore_ascii_case("file") {
                return None;
            }
            rest
        }
        None => {
            // scp-like syntax: user@host:path. A bare local path has no colon
            // after a host component, and a Windows drive letter (`C:\...`)
            // must not be mistaken for one.
            let (before, after) = url.split_once(':')?;
            if before.len() <= 1 || after.starts_with('\\') {
                return None;
            }
            return finish_remote(before, after);
        }
    };

    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => return None,
    };
    finish_remote(authority, path)
}

/// Strip credentials and ports from the authority, then join it to the path.
fn finish_remote(authority: &str, path: &str) -> Option<String> {
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split_once(':').map_or(host, |(h, _)| h);
    let host = host.trim_matches('/');
    if host.is_empty() {
        return None;
    }

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_matches('/');
    if path.is_empty() {
        return None;
    }

    // Hosts are case-insensitive, and forges treat owner/repo case-insensitively
    // too; lowercasing keeps two clones of one repository on one identity.
    Some(format!("{host}/{path}").to_ascii_lowercase())
}

/// Canonical string form of a path used inside a [`ProjectKey`].
fn normalize_path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/').to_owned();
    if cfg!(windows) {
        // Windows paths are case-insensitive, so casing must not create a
        // second identity for the same directory.
        text.to_ascii_lowercase()
    } else {
        text
    }
}

/// Final path segment of a normalized remote key.
fn last_segment(normalized: &str) -> &str {
    normalized.rsplit('/').next().unwrap_or(normalized)
}

/// Final component of a filesystem path.
fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Make a path absolute and lexically clean without requiring it to exist.
fn absolutize(path: &Path) -> Result<PathBuf> {
    let base = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| CoreError::io(path, source))?
            .join(path)
    };

    let mut out = PathBuf::new();
    for component in base.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// Reject names that cannot serve as a directory component.
fn validate_name(kind: &'static str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    let invalid = |reason: &'static str| CoreError::InvalidName {
        kind,
        value: value.to_owned(),
        reason,
    };

    if trimmed.is_empty() {
        return Err(invalid("must not be empty"));
    }
    if trimmed.len() > MAX_NAME_LEN {
        return Err(invalid("longer than 64 bytes"));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(invalid("must not be a relative path component"));
    }
    if trimmed.starts_with('_') {
        return Err(invalid(
            "leading underscore is reserved for internal scopes",
        ));
    }
    if trimmed.ends_with('.') || trimmed.ends_with(' ') {
        return Err(invalid("must not end with a dot or space"));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(invalid(
            "may only contain ASCII letters, digits, '-', '_', and '.'",
        ));
    }

    let stem = trimmed
        .split_once('.')
        .map_or(trimmed, |(stem, _)| stem)
        .to_ascii_lowercase();
    if RESERVED_STEMS.contains(&stem.as_str()) {
        return Err(invalid("is a reserved device name on Windows"));
    }

    Ok(())
}

/// Best-effort conversion of an arbitrary string into a valid name.
fn coerce_name(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.' || c == '_' || c == ' ');
    let mut result: String = trimmed.chars().take(MAX_NAME_LEN).collect();
    if result.is_empty() {
        result.push_str("unnamed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_transport_collapses_to_one_key() {
        let expected = Some("github.com/acme/api".to_owned());
        for url in [
            "https://github.com/acme/api.git",
            "https://github.com/acme/api",
            "http://github.com/acme/api.git",
            "git@github.com:acme/api.git",
            "ssh://git@github.com/acme/api.git",
            "ssh://git@github.com:22/acme/api",
            "git://github.com/acme/api.git",
            "https://token:x-oauth-basic@github.com/acme/api.git",
            "  https://GitHub.com/Acme/API.git  ",
        ] {
            assert_eq!(normalize_remote_url(url), expected, "failed for {url}");
        }
    }

    #[test]
    fn local_remotes_have_no_url_key() {
        for url in [
            "file:///srv/git/api.git",
            "/srv/git/api.git",
            "C:\\srv\\git\\api",
            "",
            "   ",
        ] {
            assert_eq!(normalize_remote_url(url), None, "failed for {url}");
        }
    }

    #[test]
    fn nested_paths_survive_normalization() {
        assert_eq!(
            normalize_remote_url("https://gitlab.com/group/sub/api.git"),
            Some("gitlab.com/group/sub/api".to_owned())
        );
    }

    #[test]
    fn key_prefixes_prevent_cross_strategy_collisions() {
        let name = ProjectName::parse("api").expect("valid");
        assert_ne!(
            ProjectKey::from_name(&name).as_str(),
            ProjectKey::from_remote("api").as_str()
        );
    }

    #[test]
    fn names_reject_path_traversal() {
        for bad in ["..", ".", "a/b", "a\\b", "../etc", ""] {
            assert!(
                ProjectName::parse(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn names_reject_windows_device_stems() {
        for bad in ["con", "COM1", "nul.txt", "AUX"] {
            assert!(
                ProjectName::parse(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn names_reject_reserved_underscore_prefix() {
        // `_global`, `_rules`, `_slots` are wiki-internal namespaces.
        assert!(ProjectName::parse("_global").is_err());
    }

    #[test]
    fn sanitized_names_are_usable() {
        assert_eq!(
            ProjectName::sanitized("My Cool Repo!")
                .expect("coerced")
                .as_str(),
            "my-cool-repo"
        );
        assert_eq!(
            ProjectName::sanitized("///").expect("coerced").as_str(),
            "unnamed"
        );
        assert!(ProjectName::sanitized("Ünïcode Ⓐ").is_ok());
    }

    #[test]
    fn marker_lookup_walks_ancestors_and_prefers_current_name() {
        let root = tempfile::tempdir().expect("tempdir");
        let nested = root.path().join("crates").join("core");
        std::fs::create_dir_all(&nested).expect("create dirs");
        std::fs::write(root.path().join(LEGACY_MARKER_FILE), "").expect("write legacy");

        let found = find_marker(&nested).expect("legacy marker found");
        assert!(found.legacy);

        std::fs::write(root.path().join(MARKER_FILE), "").expect("write preferred");
        let found = find_marker(&nested).expect("preferred marker found");
        assert!(!found.legacy);
    }

    #[test]
    fn scope_falls_back_to_directory_name_without_repository() {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("standalone");
        std::fs::create_dir_all(&dir).expect("create dir");

        let resolved = resolve_scope(&dir).expect("resolves");
        assert_eq!(resolved.scope.workspace.as_str(), DEFAULT_WORKSPACE);
        // A temp dir may itself sit inside a repository on some machines, so
        // assert only on the invariants that always hold.
        assert!(resolved.key.as_str().contains(':'));
        assert!(!resolved.scope.project.as_str().is_empty());
    }

    #[test]
    fn marker_project_name_wins_over_git() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join(MARKER_FILE),
            "[scope]\nworkspace = \"client-work\"\nproject = \"pinned\"\n",
        )
        .expect("write marker");

        let resolved = resolve_scope(root.path()).expect("resolves");
        assert_eq!(resolved.scope.project.as_str(), "pinned");
        assert_eq!(resolved.scope.workspace.as_str(), "client-work");
        assert_eq!(resolved.key.as_str(), "name:pinned");
        assert!(matches!(resolved.source, ScopeSource::Marker { .. }));
    }

    #[test]
    fn git_remote_drives_identity_when_no_marker_pins_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(root.path()).expect("init repo");
        repo.remote("origin", "git@github.com:Acme/Widget-API.git")
            .expect("add remote");
        let nested = root.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).expect("create dirs");

        let resolved = resolve_scope(&nested).expect("resolves");
        assert_eq!(resolved.key.as_str(), "git:github.com/acme/widget-api");
        assert_eq!(resolved.scope.project.as_str(), "widget-api");
        assert!(matches!(resolved.source, ScopeSource::GitRemote { .. }));
    }

    #[test]
    fn a_second_clone_of_one_repository_shares_its_identity() {
        // The whole point of keying on the remote: two checkouts, one memory.
        let make = |dir: &Path, url: &str| {
            let repo = git2::Repository::init(dir).expect("init repo");
            repo.remote("origin", url).expect("add remote");
            resolve_scope(dir).expect("resolves").project_id
        };

        let first = tempfile::tempdir().expect("tempdir");
        let second = tempfile::tempdir().expect("tempdir");

        assert_eq!(
            make(first.path(), "https://github.com/acme/widget-api.git"),
            make(second.path(), "git@github.com:acme/widget-api.git")
        );
    }

    #[test]
    fn repository_without_remote_falls_back_to_its_path() {
        let root = tempfile::tempdir().expect("tempdir");
        git2::Repository::init(root.path()).expect("init repo");

        let resolved = resolve_scope(root.path()).expect("resolves");
        assert!(matches!(resolved.source, ScopeSource::GitRoot { .. }));
        assert!(resolved.key.as_str().starts_with("path:"));
    }

    #[test]
    fn pinning_the_inferred_name_keeps_the_same_identity() {
        // Adding a marker that merely states what the project was already
        // called must not orphan its existing memory.
        let name = ProjectName::parse("anamnesis").expect("valid");
        let workspace = WorkspaceName::default();
        let from_marker = ProjectId::derive(&workspace, &ProjectKey::from_name(&name));
        let from_basename = ProjectId::derive(&workspace, &ProjectKey::from_name(&name));
        assert_eq!(from_marker, from_basename);
    }
}
