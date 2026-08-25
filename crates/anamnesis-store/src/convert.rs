//! Reading values back out of SQLite columns.
//!
//! Every module in this crate reads identifiers and timestamps out of rows,
//! and each one used to carry its own copy of these two functions. They live
//! here once instead, so the answer to "what happens when a stored value is
//! not what this crate wrote?" is decided in one place.
//!
//! Enum columns are deliberately absent: `SessionState`, `Tier`, `PageStatus`,
//! `EventKind`, and `WorkstreamStatus` each own their `as_str` /
//! `from_storage` pair in `anamnesis-core`, beside the variants they describe.
//! Re-deriving that mapping here is how it drifts.

use jiff::Timestamp;

/// Parse an identifier written by this crate.
///
/// A value here that is not a UUID means the database was edited by hand or
/// corrupted; there is no meaningful recovery, and continuing with a nil id
/// would silently attach data to the wrong row.
///
/// Generic over the identifier type so `ProjectId`, `SessionId`, `PageId`,
/// and `WorkstreamId` all share one implementation — the type is inferred
/// from the field being assigned.
pub(crate) fn parse_id<T: std::str::FromStr>(raw: String) -> T
where
    T::Err: std::fmt::Debug,
{
    raw.parse()
        .unwrap_or_else(|error| panic!("stored identifier {raw:?} is not a uuid: {error:?}"))
}

/// Parse a timestamp written by this crate.
///
/// Panics for the same reason [`parse_id`] does: every timestamp column is
/// written from Rust as RFC 3339, so a value that will not parse is a
/// corrupted row rather than an input to handle.
pub(crate) fn parse_time(raw: &str) -> Timestamp {
    raw.parse()
        .unwrap_or_else(|error| panic!("stored timestamp {raw:?} is not RFC 3339: {error:?}"))
}

/// Parse a page path written by this crate.
///
/// Panics for the same reason [`parse_id`] does: a page path is validated
/// before it is ever stored, so a stored value that fails validation means
/// the row was written by something other than this crate.
pub(crate) fn parse_page_path(raw: &str) -> anamnesis_core::page::PagePath {
    anamnesis_core::page::PagePath::parse(raw)
        .unwrap_or_else(|error| panic!("stored page path {raw:?} is invalid: {error:?}"))
}

/// A migrated in-memory index with one project registered, for tests.
///
/// Every test module in this crate needs the same four things back, and each
/// one used to build them itself. The `TempDir` is returned because it holds
/// the marker file the scope was resolved from: dropping it deletes the
/// directory, so a caller that discards it resolves a different scope on the
/// next call.
#[cfg(test)]
pub(crate) fn fixture() -> (
    tempfile::TempDir,
    crate::Store,
    anamnesis_core::ids::ProjectId,
    anamnesis_core::ids::WorkspaceId,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(".anamnesis.toml"),
        "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
    )
    .expect("marker");
    let scope = anamnesis_core::scope::resolve_scope(dir.path()).expect("scope");

    let store = crate::Store::open_in_memory().expect("open");
    store.migrate().expect("migrate");
    store
        .upsert_project(&scope, fixture_now())
        .expect("project");

    (dir, store, scope.project_id, scope.workspace_id)
}

/// The instant every fixture-based test treats as "now".
///
/// Fixed rather than `Timestamp::now()` so a test asserting on ordering or
/// decay never depends on when it happened to run.
#[cfg(test)]
pub(crate) fn fixture_now() -> Timestamp {
    "2026-08-24T09:00:00Z".parse().expect("timestamp")
}
