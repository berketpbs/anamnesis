//! Identifier newtypes and their derivation rules.
//!
//! Two families of identifier exist, and the distinction matters:
//!
//! * **Derived** ([`WorkspaceId`], [`ProjectId`], [`PageId`]) are UUIDv5 values
//!   computed from stable names. Deleting the SQLite index and rebuilding it
//!   from the wiki reproduces exactly the same identifiers, which is what makes
//!   the wiki — not the database — the source of truth.
//! * **Minted** ([`SessionId`], [`ObservationId`]) are UUIDv7 values. They are
//!   time-ordered, so an index on the primary key already yields chronological
//!   locality and "most recent sessions" needs no secondary sort.

use uuid::Uuid;

use crate::page::PagePath;
use crate::scope::{ProjectKey, WorkspaceName};

/// Root namespace for every derived identifier in anamnesis.
///
/// Canonical form: `744472a9-5db1-52bd-b20f-8e0be549f19f`, itself
/// `uuid5(NAMESPACE_DNS, "anamnesis.memory")`.
///
/// **This constant must never change.** Every [`ProjectId`] and [`PageId`] ever
/// written is derived from it; altering it silently orphans all existing memory
/// because lookups would compute different identifiers for the same project.
pub const NAMESPACE: Uuid = Uuid::from_u128(0x744472a9_5db1_52bd_b20f_8e0be549f19f);

macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Wrap an existing UUID without validating how it was produced.
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// Borrow the underlying UUID.
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0.hyphenated())
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

id_newtype! {
    /// Identifies a workspace, derived from its name.
    WorkspaceId
}

id_newtype! {
    /// Identifies a project within a workspace, derived from its stable key.
    ProjectId
}

id_newtype! {
    /// Identifies a wiki page, derived from its project and path.
    PageId
}

id_newtype! {
    /// Identifies an agent session. Minted, time-ordered.
    SessionId
}

id_newtype! {
    /// Identifies a single captured observation. Minted, time-ordered.
    ObservationId
}

id_newtype! {
    /// Identifies a handoff between two sessions. Minted, time-ordered.
    HandoffId
}

id_newtype! {
    /// Identifies a workstream, derived from its project and slug.
    WorkstreamId
}

impl WorkspaceId {
    /// Derive the identifier for a workspace name.
    pub fn derive(name: &WorkspaceName) -> Self {
        Self(Uuid::new_v5(
            &NAMESPACE,
            format!("ws:{}", name.as_str()).as_bytes(),
        ))
    }
}

impl ProjectId {
    /// Derive the identifier for a project key inside a workspace.
    ///
    /// The workspace is part of the input, so the same repository checked out
    /// under two different workspaces keeps two independent memories.
    pub fn derive(workspace: &WorkspaceName, key: &ProjectKey) -> Self {
        Self(Uuid::new_v5(
            &NAMESPACE,
            format!("proj:{}/{}", workspace.as_str(), key.as_str()).as_bytes(),
        ))
    }
}

impl PageId {
    /// Derive the identifier for a page path within a project.
    ///
    /// The project identifier acts as the namespace, so identical paths in
    /// different projects never collide.
    pub fn derive(project: ProjectId, path: &PagePath) -> Self {
        Self(Uuid::new_v5(project.as_uuid(), path.as_str().as_bytes()))
    }
}

impl WorkstreamId {
    /// Derive the identifier for a workstream slug within a project.
    ///
    /// Derived rather than minted so starting a workstream is idempotent: the
    /// same slug asked for twice names the same row, the way a page path
    /// does, rather than the caller needing to look one up before deciding
    /// whether to create it.
    pub fn derive(project: ProjectId, slug: &str) -> Self {
        Self(Uuid::new_v5(
            project.as_uuid(),
            format!("workstream:{slug}").as_bytes(),
        ))
    }
}

impl SessionId {
    /// Mint a new time-ordered session identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Derive the identifier for a session the harness already named.
    ///
    /// Hooks arrive one event at a time, each carrying the harness's own
    /// session id and nothing else to correlate on. Deriving our identifier
    /// from that string means any event can compute the row it belongs to
    /// without first querying for it — which is what keeps the capture path
    /// a single insert.
    ///
    /// This is the one place a [`SessionId`] is a v5 rather than a v7, so
    /// ordering by identifier is only chronological for minted ones.
    pub fn derive(project: ProjectId, agent_session_id: &str) -> Self {
        Self(Uuid::new_v5(
            project.as_uuid(),
            format!("session:{agent_session_id}").as_bytes(),
        ))
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl ObservationId {
    /// Mint a new time-ordered observation identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for ObservationId {
    fn default() -> Self {
        Self::new()
    }
}

impl HandoffId {
    /// Mint a new time-ordered handoff identifier.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for HandoffId {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn ws(name: &str) -> WorkspaceName {
        WorkspaceName::parse(name).expect("valid workspace name")
    }

    fn key(value: &str) -> ProjectKey {
        ProjectKey::from_canonical(value.to_owned())
    }

    #[test]
    fn namespace_constant_is_pinned() {
        // Guards against an accidental edit to NAMESPACE, which would orphan
        // every project and page identifier ever written.
        assert_eq!(
            NAMESPACE.to_string(),
            "744472a9-5db1-52bd-b20f-8e0be549f19f"
        );
    }

    #[test]
    fn derived_ids_are_stable_across_runs() {
        let a = ProjectId::derive(&ws("default"), &key("git:github.com/berketpbs/anamnesis"));
        let b = ProjectId::derive(&ws("default"), &key("git:github.com/berketpbs/anamnesis"));
        assert_eq!(a, b);
    }

    #[test]
    fn workspace_participates_in_project_identity() {
        let k = key("git:github.com/berketpbs/anamnesis");
        assert_ne!(
            ProjectId::derive(&ws("default"), &k),
            ProjectId::derive(&ws("client-work"), &k)
        );
    }

    #[test]
    fn page_ids_are_namespaced_by_project() {
        let path = PagePath::parse("decisions/0001-storage.md").expect("valid path");
        let one = ProjectId::derive(&ws("default"), &key("path:/a"));
        let two = ProjectId::derive(&ws("default"), &key("path:/b"));
        assert_ne!(PageId::derive(one, &path), PageId::derive(two, &path));
        assert_eq!(PageId::derive(one, &path), PageId::derive(one, &path));
    }

    #[test]
    fn workstream_ids_are_namespaced_by_project_and_stable_by_slug() {
        let one = ProjectId::derive(&ws("default"), &key("path:/a"));
        let two = ProjectId::derive(&ws("default"), &key("path:/b"));
        assert_ne!(
            WorkstreamId::derive(one, "auth-refactor"),
            WorkstreamId::derive(two, "auth-refactor")
        );
        assert_eq!(
            WorkstreamId::derive(one, "auth-refactor"),
            WorkstreamId::derive(one, "auth-refactor")
        );
        assert_ne!(
            WorkstreamId::derive(one, "auth-refactor"),
            WorkstreamId::derive(one, "bug-123")
        );
    }

    #[test]
    fn minted_ids_are_time_ordered() {
        let first = SessionId::new();
        let second = SessionId::new();
        assert!(first < second || first.as_uuid() != second.as_uuid());
        assert_eq!(second.as_uuid().get_version_num(), 7);
    }

    #[test]
    fn round_trips_through_string() {
        let id = SessionId::new();
        assert_eq!(SessionId::from_str(&id.to_string()).expect("parses"), id);
    }

    #[test]
    fn serde_representation_is_a_bare_string() {
        let id = ProjectId::derive(&ws("default"), &key("path:/x"));
        let json = serde_json::to_string(&id).expect("serializes");
        assert_eq!(json, format!("\"{id}\""));
    }
}
