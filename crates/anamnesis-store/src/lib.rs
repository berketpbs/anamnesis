//! Persistence for the anamnesis memory system: the SQLite index, and the
//! raw spool behind it.
//!
//! This database is an *index*, never the source of truth. Every page it holds
//! is rebuildable from the markdown wiki, and page identifiers are derived from
//! `(project, path)` rather than minted, so a rebuild reproduces them exactly.
//! That is what makes "delete the database and reindex" a safe operation rather
//! than a data loss event.
//!
//! Pages are not the whole story, though: the *observations* a page was
//! compiled from exist in no wiki. [`RawSpool`] is what keeps them
//! rebuildable too, writing every captured event to append-only JSONL under
//! `<data_dir>/raw/` at the moment it arrives.
//!
//! Writes are serialised through a single connection behind a mutex. SQLite
//! permits one writer at a time regardless; making that explicit turns a
//! runtime `SQLITE_BUSY` into a wait rather than an error.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::path::Path;

use anamnesis_core::scope::ResolvedScope;
use jiff::Timestamp;
use parking_lot::{Mutex, MutexGuard};
use rusqlite::Connection;

mod embedded {
    refinery::embed_migrations!("migrations");
}

mod convert;
mod ops;
mod query;
mod raw;
mod sweep;
mod workstream;

pub use ops::{SessionSummary, new_handoff, new_observation, new_session};
pub use query::PageHit;
pub use raw::{RawError, RawRecord, RawSpool};
pub use sweep::SweepRow;
pub use workstream::{WorkstreamHandoff, WorkstreamSession};

/// Errors produced by the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A SQLite operation failed.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A schema migration failed.
    #[error("migration error: {0}")]
    Migration(Box<refinery::Error>),
}

impl From<refinery::Error> for StoreError {
    fn from(source: refinery::Error) -> Self {
        Self::Migration(Box::new(source))
    }
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, StoreError>;

/// How long a blocked writer waits before giving up.
const BUSY_TIMEOUT_MS: u32 = 5_000;

/// An open SQLite index.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open, or create, the index at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path.as_ref())?)
    }

    /// Open a private in-memory index, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Apply the pragmas every connection needs, then take ownership.
    fn from_connection(conn: Connection) -> Result<Self> {
        // `journal_mode` returns the mode it settled on, so it has to be run as
        // a query; `pragma_update` rejects statements that produce rows.
        // In-memory databases cannot use WAL and answer "memory" instead, which
        // is not an error.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "cache_size", -64_000)?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS.into()))?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Bring the schema up to date.
    ///
    /// Safe to call on every startup: refinery records what it has applied and
    /// skips it next time.
    pub fn migrate(&self) -> Result<()> {
        let mut conn = self.conn.lock();
        embedded::migrations::runner().run(&mut *conn)?;
        Ok(())
    }

    /// Version of the most recently applied migration, if any.
    pub fn schema_version(&self) -> Result<Option<i64>> {
        let conn = self.conn.lock();
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'refinery_schema_history')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let version = conn.query_row(
            "SELECT MAX(version) FROM refinery_schema_history",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(version)
    }

    /// Borrow the connection.
    ///
    /// Holding this guard blocks every other reader and writer, so keep the
    /// scope tight.
    pub fn connection(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    /// Record a project, or refresh the one already recorded under its id.
    ///
    /// The identifier is derived, not minted, so re-running this for the same
    /// repository updates the existing row instead of creating a second one —
    /// including after the database has been deleted and rebuilt.
    pub fn upsert_project(&self, scope: &ResolvedScope, now: Timestamp) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO projects
                 (id, workspace_id, workspace, name, project_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT (id) DO UPDATE SET
                 workspace   = excluded.workspace,
                 name        = excluded.name,
                 project_key = excluded.project_key,
                 updated_at  = excluded.updated_at",
            rusqlite::params![
                scope.project_id.to_string(),
                scope.workspace_id.to_string(),
                scope.scope.workspace.as_str(),
                scope.scope.project.as_str(),
                scope.key.as_str(),
                now.to_string(),
            ],
        )?;
        Ok(())
    }

    /// How many projects the index knows about.
    pub fn project_count(&self) -> Result<i64> {
        let conn = self.conn.lock();
        Ok(conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn migrated() -> Store {
        let store = Store::open_in_memory().expect("open");
        store.migrate().expect("migrate");
        store
    }

    fn seed_project(store: &Store) -> String {
        let conn = store.connection();
        conn.execute(
            "INSERT INTO projects (id, workspace_id, workspace, name, project_key, created_at, updated_at)
             VALUES (?1, ?2, 'default', 'anamnesis', 'git:github.com/acme/api', ?3, ?3)",
            params!["proj-1", "ws-1", "2026-08-19T00:00:00Z"],
        )
        .expect("insert project");
        "proj-1".to_owned()
    }

    fn seed_page(store: &Store, id: &str, path: &str, title: &str, body: &str) {
        let conn = store.connection();
        conn.execute(
            "INSERT INTO pages (id, project_id, path, title, body, tier, status, created_at, updated_at)
             VALUES (?1, 'proj-1', ?2, ?3, ?4, 'semantic', 'active', ?5, ?5)",
            params![id, path, title, body, "2026-08-19T00:00:00Z"],
        )
        .expect("insert page");
    }

    #[test]
    fn migrations_run_and_report_a_version() {
        let store = Store::open_in_memory().expect("open");
        assert_eq!(store.schema_version().expect("version"), None);

        store.migrate().expect("migrate");
        assert_eq!(store.schema_version().expect("version"), Some(6));
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let store = migrated();
        store.migrate().expect("second migrate");
        assert_eq!(store.schema_version().expect("version"), Some(6));
    }

    #[test]
    fn every_expected_table_exists() {
        let store = migrated();
        let conn = store.connection();
        for table in [
            "projects",
            "sessions",
            "observations",
            "pages",
            "pages_fts",
            "entities",
            "page_entities",
            "page_links",
            "handoffs",
            "page_feedback",
            "page_embeddings",
            "workstreams",
        ] {
            let found: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("query");
            assert_eq!(found, 1, "table {table} is missing");
        }
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let store = migrated();
        let conn = store.connection();
        let result = conn.execute(
            "INSERT INTO sessions (id, project_id, agent, checkout_path, state, started_at)
             VALUES ('s1', 'no-such-project', 'claude-code', '/tmp', 'open', ?1)",
            params!["2026-08-19T00:00:00Z"],
        );
        assert!(result.is_err(), "orphan session should be rejected");
    }

    #[test]
    fn deleting_a_project_cascades_to_its_sessions() {
        let store = migrated();
        seed_project(&store);
        let conn = store.connection();
        conn.execute(
            "INSERT INTO sessions (id, project_id, agent, checkout_path, state, started_at)
             VALUES ('s1', 'proj-1', 'claude-code', '/tmp', 'open', ?1)",
            params!["2026-08-19T00:00:00Z"],
        )
        .expect("insert session");
        conn.execute("DELETE FROM projects WHERE id = 'proj-1'", [])
            .expect("delete project");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .expect("count");
        assert_eq!(remaining, 0);
    }

    #[test]
    fn unknown_agents_are_accepted_without_a_migration() {
        // The point of keeping `agent` free text: a new harness must not need a
        // schema change.
        let store = migrated();
        seed_project(&store);
        let conn = store.connection();
        conn.execute(
            "INSERT INTO sessions (id, project_id, agent, checkout_path, state, started_at)
             VALUES ('s1', 'proj-1', 'some-brand-new-harness', '/tmp', 'open', ?1)",
            params!["2026-08-19T00:00:00Z"],
        )
        .expect("unknown agent should be accepted");
    }

    #[test]
    fn invalid_tiers_and_statuses_are_rejected() {
        let store = migrated();
        seed_project(&store);
        let conn = store.connection();
        for (tier, status) in [("nonsense", "active"), ("semantic", "nonsense")] {
            let result = conn.execute(
                "INSERT INTO pages (id, project_id, path, title, body, tier, status, created_at, updated_at)
                 VALUES ('p-bad', 'proj-1', 'x.md', 'x', 'x', ?1, ?2, ?3, ?3)",
                params![tier, status, "2026-08-19T00:00:00Z"],
            );
            assert!(result.is_err(), "{tier}/{status} should be rejected");
        }
    }

    #[test]
    fn full_text_search_tracks_page_writes() {
        let store = migrated();
        seed_project(&store);
        seed_page(
            &store,
            "p1",
            "decisions/0001-storage.md",
            "Storage engine",
            "we chose sqlite for the index",
        );

        let conn = store.connection();
        let hits = |conn: &Connection, query: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH ?1",
                [query],
                |row| row.get(0),
            )
            .expect("search")
        };

        assert_eq!(hits(&conn, "sqlite"), 1);

        conn.execute(
            "UPDATE pages SET body = 'we chose postgres instead' WHERE id = 'p1'",
            [],
        )
        .expect("update");
        assert_eq!(hits(&conn, "sqlite"), 0);
        assert_eq!(hits(&conn, "postgres"), 1);

        conn.execute("DELETE FROM pages WHERE id = 'p1'", [])
            .expect("delete");
        assert_eq!(hits(&conn, "postgres"), 0);
    }

    #[test]
    fn search_folds_diacritics_but_not_distinct_letters() {
        let store = migrated();
        seed_project(&store);
        seed_page(&store, "p1", "notes/tr.md", "Kayıt", "veritabanı seçimi");

        let conn = store.connection();
        let hits = |query: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH ?1",
                [query],
                |row| row.get(0),
            )
            .expect("search")
        };

        // `ç` is `c` plus a combining cedilla, so unicode61 folds it.
        assert_eq!(
            hits("secimi"),
            1,
            "an unaccented query should match 'seçimi'"
        );

        // `ı` (U+0131) is not `i` with a mark removed — it is its own letter and
        // does not decompose, so diacritic folding cannot reach it. Turkish
        // queries have to be spelled as written; this is a property of the
        // tokenizer, not a bug to chase.
        assert_eq!(hits("veritabani"), 0);
        assert_eq!(hits("veritabanı"), 1);
    }

    #[test]
    fn supersession_survives_the_superseded_page_being_removed() {
        let store = migrated();
        seed_project(&store);
        seed_page(&store, "p1", "decisions/a.md", "Old", "old body");
        seed_page(&store, "p2", "decisions/b.md", "New", "new body");

        let conn = store.connection();
        conn.execute(
            "UPDATE pages SET supersedes = 'p1', is_latest = 1 WHERE id = 'p2'",
            [],
        )
        .expect("link supersession");
        conn.execute(
            "UPDATE pages SET is_latest = 0, status = 'superseded' WHERE id = 'p1'",
            [],
        )
        .expect("mark old");

        conn.execute("DELETE FROM pages WHERE id = 'p1'", [])
            .expect("delete old page");
        let dangling: Option<String> = conn
            .query_row("SELECT supersedes FROM pages WHERE id = 'p2'", [], |row| {
                row.get(0)
            })
            .expect("read");
        assert_eq!(dangling, None, "the pointer should be cleared, not dangle");
    }

    #[test]
    fn only_one_handoff_can_be_pending_per_project() {
        let store = migrated();
        seed_project(&store);
        let conn = store.connection();
        conn.execute(
            "INSERT INTO sessions (id, project_id, agent, checkout_path, state, started_at)
             VALUES ('s1', 'proj-1', 'claude-code', '/tmp', 'closed', ?1)",
            params!["2026-08-19T00:00:00Z"],
        )
        .expect("insert session");

        let insert = |id: &str| {
            conn.execute(
                "INSERT INTO handoffs (id, project_id, from_session, body, state, created_at)
                 VALUES (?1, 'proj-1', 's1', 'carry on', 'pending', ?2)",
                params![id, "2026-08-19T00:00:00Z"],
            )
        };

        insert("h1").expect("first pending handoff");
        assert!(
            insert("h2").is_err(),
            "second pending handoff must be rejected"
        );

        conn.execute("UPDATE handoffs SET state = 'expired' WHERE id = 'h1'", [])
            .expect("expire");
        insert("h3").expect("a new handoff is allowed once the old one is expired");
    }

    #[test]
    fn unresolved_links_are_recorded_rather_than_dropped() {
        let store = migrated();
        seed_project(&store);
        seed_page(&store, "p1", "notes/a.md", "A", "see [[not-written-yet]]");

        let conn = store.connection();
        conn.execute(
            "INSERT INTO page_links (from_page_id, to_target) VALUES ('p1', 'not-written-yet')",
            [],
        )
        .expect("insert link");

        let unresolved: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM page_links WHERE to_page_id IS NULL",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(unresolved, 1);
    }

    #[test]
    fn upserting_a_project_twice_keeps_one_row() {
        use anamnesis_core::scope::resolve_scope;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(".anamnesis.toml"),
            "[scope]\nworkspace = \"default\"\nproject = \"widget\"\n",
        )
        .expect("write marker");
        let scope = resolve_scope(dir.path()).expect("resolve");

        let store = migrated();
        let now = "2026-08-19T00:00:00Z".parse::<Timestamp>().expect("parse");
        store.upsert_project(&scope, now).expect("first upsert");
        store.upsert_project(&scope, now).expect("second upsert");

        assert_eq!(store.project_count().expect("count"), 1);
    }

    #[test]
    fn a_file_backed_index_survives_reopening() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("anamnesis.db");

        {
            let store = Store::open(&path).expect("open");
            store.migrate().expect("migrate");
            seed_project(&store);
        }

        let store = Store::open(&path).expect("reopen");
        store.migrate().expect("migrate again");
        let conn = store.connection();
        let projects: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .expect("count");
        assert_eq!(projects, 1);
    }
}
