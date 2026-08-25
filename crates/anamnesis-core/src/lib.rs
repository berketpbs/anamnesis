//! Core types and abstractions for the anamnesis memory system.
//!
//! This crate owns data, validation, and identity — no I/O beyond reading a
//! marker file and asking git for a remote URL, and no storage traits. Concrete
//! persistence lives in `anamnesis-store`; concrete wiki handling lives in
//! `anamnesis-wiki`.
//!
//! Two invariants shape everything here:
//!
//! * **The wiki is the source of truth.** Identifiers for projects and pages
//!   are derived from stable names ([`ids`]), so a SQLite index can be deleted
//!   and rebuilt from markdown without anything losing its identity.
//! * **Validated names are a containment boundary.** [`scope::WorkspaceName`],
//!   [`scope::ProjectName`], and [`page::PagePath`] all become filesystem
//!   paths, so nothing constructed through them can escape the wiki root.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod capture;
pub mod config;
pub mod datadir;
pub mod decay;
pub mod error;
pub mod handoff;
pub mod ids;
pub mod observation;
pub mod page;
pub mod retrieval;
pub mod sanitize;
pub mod scope;
pub mod session;
pub mod workstream;

pub use error::{CoreError, Result};

/// Commonly used types, for `use anamnesis_core::prelude::*;`.
pub mod prelude {
    pub use crate::config::MarkerConfig;
    pub use crate::datadir::DataDir;
    pub use crate::decay::{DecayInputs, DecayParams, retention_score};
    pub use crate::error::{CoreError, Result};
    pub use crate::handoff::{Handoff, HandoffState};
    pub use crate::ids::{
        HandoffId, ObservationId, PageId, ProjectId, SessionId, WorkspaceId, WorkstreamId,
    };
    pub use crate::observation::{BoundedBody, EventKind, Observation, ToolRef};
    pub use crate::page::{Entity, Frontmatter, Page, PagePath, PageStatus, Tier};
    pub use crate::retrieval::{
        RRF_K, authority_multiplier, fuse_and_rank, reciprocal_rank_fusion,
    };
    pub use crate::sanitize::{Redacted, Redactor};
    pub use crate::scope::{
        ProjectKey, ProjectName, ResolvedScope, Scope, ScopeSource, WorkspaceName, resolve_scope,
    };
    pub use crate::session::{AgentKind, Session, SessionState};
    pub use crate::workstream::{Workstream, WorkstreamSlug, WorkstreamStatus};
}
