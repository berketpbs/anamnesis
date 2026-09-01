//! Moving a project's memory to a new identity.
//!
//! A project's identifier is *derived* — from its workspace and the key its
//! name or git remote produces — and so is every page's, from that identifier
//! and the page's path. That is what lets two clones of one repository share
//! one memory without configuring anything, and it is also what makes renaming
//! a repository look like losing everything: the next session resolves a
//! different key, finds an empty project, and nothing says the old one is
//! still there under a name nobody types any more.
//!
//! This is that repair. It is a migration rather than an update because the
//! identifiers cannot be edited in place: each page's id has to be re-derived
//! from the new project, and every row that points at a page has to follow it
//! in the same breath. Nine tables move together or none of them do.
//!
//! Two rules hold it together.
//!
//! **One transaction, with foreign keys deferred.** Half of a rename is worse
//! than none: a page whose entities were left behind is a page retrieval ranks
//! wrongly and nothing reports. Deferring the checks to the commit is what
//! lets the parent row move before its children without SQLite refusing each
//! step on the way.
//!
//! **The audit log comes too.** Its rows carry a project id and no foreign
//! key, so nothing would move them; but they are the record of what happened
//! to *this* memory, and leaving them behind would strand the history under an
//! identifier the project no longer has.

use anamnesis_core::ids::{PageId, ProjectId};
use anamnesis_core::page::PagePath;
use anamnesis_core::scope::{ProjectKey, ProjectName};
use jiff::Timestamp;
use rusqlite::params;

use crate::Store;
use crate::convert::parse_id;

/// What a rename moved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Renamed {
    /// Pages whose identifiers were re-derived.
    pub pages: i64,
    /// Sessions carried across.
    pub sessions: i64,
    /// Audit lines that followed the memory they describe.
    pub audit_lines: i64,
}

/// Why a rename cannot go ahead.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// There is nothing recorded under the old identity.
    #[error("nothing is recorded under the current identity")]
    NothingToMove,

    /// Something is already recorded under the new one.
    ///
    /// Merging two memories is a different operation with different answers —
    /// which page wins where both have `decisions.md`, whose access counts
    /// survive — and doing it silently as part of a rename would be the worst
    /// way to decide them.
    #[error("{0} already has memory of its own; renaming into it would merge two projects")]
    AlreadyThere(String),

    /// The database refused.
    #[error(transparent)]
    Store(#[from] crate::StoreError),
}

impl Store {
    /// Move everything recorded under `old` to `new`.
    pub fn rename_project(
        &self,
        old: ProjectId,
        new: ProjectId,
        name: &ProjectName,
        key: &ProjectKey,
        now: Timestamp,
    ) -> Result<Renamed, RenameError> {
        let mut conn = self.connection();

        let exists =
            |id: ProjectId, conn: &rusqlite::Connection| -> Result<bool, crate::StoreError> {
                Ok(conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM projects WHERE id = ?1)",
                    params![id.to_string()],
                    |row| row.get(0),
                )?)
            };
        if !exists(old, &conn)? {
            return Err(RenameError::NothingToMove);
        }
        if old != new && exists(new, &conn)? {
            return Err(RenameError::AlreadyThere(name.to_string()));
        }

        let tx = conn.transaction().map_err(crate::StoreError::from)?;
        // Checked at the commit rather than at each statement: the parent row
        // has to move before its children can point at it, and no ordering of
        // the two satisfies an immediate check.
        tx.pragma_update(None, "defer_foreign_keys", true)
            .map_err(crate::StoreError::from)?;

        // Every page, with the path its new identifier is derived from.
        let pages: Vec<(PageId, PagePath)> = {
            let mut statement = tx
                .prepare("SELECT id, path FROM pages WHERE project_id = ?1")
                .map_err(crate::StoreError::from)?;
            let rows = statement
                .query_map(params![old.to_string()], |row| {
                    let id: String = row.get(0)?;
                    let path: String = row.get(1)?;
                    Ok((id, path))
                })
                .map_err(crate::StoreError::from)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(crate::StoreError::from)?
                .into_iter()
                .map(|(id, path)| {
                    (
                        parse_id::<PageId>(id),
                        PagePath::parse(&path).expect("a stored page path was written by us"),
                    )
                })
                .collect()
        };

        let old_id = old.to_string();
        let new_id = new.to_string();

        // The project itself, then everything keyed to it.
        tx.execute(
            "UPDATE projects SET id = ?2, name = ?3, project_key = ?4, updated_at = ?5
             WHERE id = ?1",
            params![old_id, new_id, name.as_str(), key.as_str(), now.to_string()],
        )
        .map_err(crate::StoreError::from)?;

        let mut moved = Renamed::default();
        for table in [
            "sessions",
            "pages",
            "entities",
            "handoffs",
            "workstreams",
            "proposals",
        ] {
            let count = tx
                .execute(
                    &format!("UPDATE {table} SET project_id = ?2 WHERE project_id = ?1"),
                    params![old_id, new_id],
                )
                .map_err(crate::StoreError::from)?;
            if table == "sessions" {
                moved.sessions = count as i64;
            }
        }
        tx.execute(
            "UPDATE page_links SET to_project_id = ?2 WHERE to_project_id = ?1",
            params![old_id, new_id],
        )
        .map_err(crate::StoreError::from)?;

        // The record of what happened to this memory travels with it. Nothing
        // else would move these: `audit_log` has no foreign key, on purpose.
        moved.audit_lines = tx
            .execute(
                "UPDATE audit_log SET project_id = ?2 WHERE project_id = ?1",
                params![old_id, new_id],
            )
            .map_err(crate::StoreError::from)? as i64;

        // Then each page, whose identifier is derived and therefore changes.
        for (was, path) in &pages {
            let becomes = PageId::derive(new, path);
            let (was, becomes) = (was.to_string(), becomes.to_string());
            for statement in [
                "UPDATE pages SET id = ?2 WHERE id = ?1",
                "UPDATE pages SET supersedes = ?2 WHERE supersedes = ?1",
                "UPDATE page_entities SET page_id = ?2 WHERE page_id = ?1",
                "UPDATE page_links SET from_page_id = ?2 WHERE from_page_id = ?1",
                "UPDATE page_links SET to_page_id = ?2 WHERE to_page_id = ?1",
                "UPDATE page_embeddings SET page_id = ?2 WHERE page_id = ?1",
                "UPDATE page_feedback SET page_id = ?2 WHERE page_id = ?1",
            ] {
                tx.execute(statement, params![was, becomes])
                    .map_err(crate::StoreError::from)?;
            }
            moved.pages += 1;
        }

        tx.commit().map_err(crate::StoreError::from)?;
        Ok(moved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::audit::{Action, AuditEntry, Via};
    use anamnesis_core::page::{Frontmatter, Page};
    use anamnesis_core::scope::{ResolvedScope, resolve_scope};
    use anamnesis_core::session::AgentKind;

    fn at(raw: &str) -> Timestamp {
        raw.parse().expect("timestamp")
    }

    fn scope_named(name: &str) -> (tempfile::TempDir, ResolvedScope) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(".anamnesis.toml"),
            format!("[scope]\nworkspace = \"default\"\nproject = \"{name}\"\n"),
        )
        .expect("marker");
        let scope = resolve_scope(dir.path()).expect("scope");
        (dir, scope)
    }

    /// A project with everything a rename has to carry: pages that link to
    /// each other, a session, an entity, a handoff, and an audit line.
    fn populated(store: &Store, scope: &ResolvedScope, root: &std::path::Path) {
        store
            .upsert_project(scope, at("2026-09-02T09:00:00Z"))
            .expect("project");

        let session = crate::new_session(
            anamnesis_core::ids::SessionId::new(),
            scope.project_id,
            scope.workspace_id,
            AgentKind::ClaudeCode,
            root.to_path_buf(),
            at("2026-09-02T09:00:00Z"),
            None,
        );
        store.ensure_session(&session).expect("session");

        let mut decisions = Frontmatter::new("Decisions", Vec::new()).expect("frontmatter");
        decisions.entities = vec![anamnesis_core::page::Entity::parse("sqlite").expect("entity")];
        let decisions = Page::new(
            scope.project_id,
            PagePath::parse("notes/decisions.md").expect("path"),
            decisions,
            "we chose sqlite; see [[notes/why.md]]",
        );
        let why = Page::new(
            scope.project_id,
            PagePath::parse("notes/why.md").expect("path"),
            Frontmatter::new("Why", Vec::new()).expect("frontmatter"),
            "one file, no server",
        );
        for page in [&why, &decisions] {
            store
                .index_page(
                    scope.project_id,
                    page,
                    &links_in(&page.body),
                    None,
                    at("2026-09-02T09:02:00Z"),
                )
                .expect("index");
        }

        store
            .append_audit(
                &AuditEntry::new(
                    Action::PageWritten,
                    Via::Cli,
                    "notes/decisions.md",
                    at("2026-09-02T09:03:00Z"),
                )
                .in_project(scope.project_id),
            )
            .expect("audit");
    }

    /// The `[[wiki link]]` targets in a body, without depending on the wiki
    /// crate from the crate underneath it.
    fn links_in(body: &str) -> Vec<String> {
        body.split("[[")
            .skip(1)
            .filter_map(|rest| rest.split_once("]]"))
            .map(|(target, _)| target.to_owned())
            .collect()
    }

    fn store() -> Store {
        let store = Store::open_in_memory().expect("store");
        store.migrate().expect("migrate");
        store
    }

    #[test]
    fn everything_recorded_moves_to_the_new_identity() {
        let store = store();
        let (dir, old) = scope_named("widget");
        populated(&store, &old, dir.path());
        let (_new_dir, new) = scope_named("gadget");

        let moved = store
            .rename_project(
                old.project_id,
                new.project_id,
                &new.scope.project,
                &new.key,
                at("2026-09-02T10:00:00Z"),
            )
            .expect("rename");

        assert_eq!(moved.pages, 2);
        assert_eq!(moved.sessions, 1);
        assert_eq!(moved.audit_lines, 1);

        assert_eq!(store.page_count(old.project_id).expect("old"), 0);
        assert_eq!(store.page_count(new.project_id).expect("new"), 2);
        assert_eq!(store.session_count(new.project_id).expect("new"), 1);
        assert_eq!(
            store
                .audit_trail(Some(new.project_id), 10)
                .expect("trail")
                .len(),
            1
        );
    }

    /// The identifiers are derived, so a moved page is only findable if its id
    /// was re-derived from the new project. A page whose row moved but whose
    /// id did not would be invisible to every lookup by path.
    #[test]
    fn a_moved_page_is_findable_under_its_new_identity() {
        let store = store();
        let (dir, old) = scope_named("widget");
        populated(&store, &old, dir.path());
        let (_new_dir, new) = scope_named("gadget");

        store
            .rename_project(
                old.project_id,
                new.project_id,
                &new.scope.project,
                &new.key,
                at("2026-09-02T10:00:00Z"),
            )
            .expect("rename");

        let path = PagePath::parse("notes/decisions.md").expect("path");
        let expected = PageId::derive(new.project_id, &path);
        let found: Vec<(PageId, String)> = store
            .page_paths(new.project_id)
            .expect("paths")
            .into_iter()
            .filter(|(id, _)| *id == expected)
            .collect();

        assert_eq!(
            found.len(),
            1,
            "the page did not come back under the identity its new project derives"
        );
        assert_eq!(found[0].1, "notes/decisions.md");
    }

    /// Half a rename is worse than none: a page whose links were left behind
    /// is a page retrieval ranks wrongly and nothing reports.
    #[test]
    fn the_links_between_pages_survive_the_move() {
        let store = store();
        let (dir, old) = scope_named("widget");
        populated(&store, &old, dir.path());
        let (_new_dir, new) = scope_named("gadget");

        store
            .rename_project(
                old.project_id,
                new.project_id,
                &new.scope.project,
                &new.key,
                at("2026-09-02T10:00:00Z"),
            )
            .expect("rename");

        let from = PageId::derive(
            new.project_id,
            &PagePath::parse("notes/decisions.md").expect("path"),
        );
        let conn = store.connection();
        let links: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM page_links WHERE from_page_id = ?1",
                params![from.to_string()],
                |row| row.get(0),
            )
            .expect("count");

        assert_eq!(links, 1, "the link left the page behind");
    }

    /// Merging two memories is a different operation with different answers —
    /// which page wins where both have `decisions.md`, whose read counts
    /// survive — and deciding them silently inside a rename would be the worst
    /// possible way to decide them.
    #[test]
    fn renaming_into_a_project_that_already_has_memory_is_refused() {
        let store = store();
        let (mine_dir, mine) = scope_named("widget");
        populated(&store, &mine, mine_dir.path());
        let (theirs_dir, theirs) = scope_named("gadget");
        populated(&store, &theirs, theirs_dir.path());

        let refused = store.rename_project(
            mine.project_id,
            theirs.project_id,
            &theirs.scope.project,
            &theirs.key,
            at("2026-09-02T10:00:00Z"),
        );

        assert!(matches!(refused, Err(RenameError::AlreadyThere(_))));
        assert_eq!(store.page_count(mine.project_id).expect("mine"), 2);
        assert_eq!(store.page_count(theirs.project_id).expect("theirs"), 2);
    }

    #[test]
    fn renaming_a_project_that_was_never_recorded_says_so() {
        let store = store();
        let (_dir, old) = scope_named("widget");
        let (_new_dir, new) = scope_named("gadget");

        let refused = store.rename_project(
            old.project_id,
            new.project_id,
            &new.scope.project,
            &new.key,
            at("2026-09-02T10:00:00Z"),
        );

        assert!(matches!(refused, Err(RenameError::NothingToMove)));
    }
}
