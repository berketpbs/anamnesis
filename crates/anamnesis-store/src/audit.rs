//! Reading and writing the audit log.
//!
//! Two operations, and the asymmetry between them is the point: lines go in
//! one at a time from wherever a change is made, and come out newest-first for
//! somebody asking what happened. Nothing here updates or deletes — an audit
//! log that can be edited answers a different, much weaker question.
//!
//! Failing to write a line never fails the change it describes. That is a
//! deliberate trade in the same direction as the raw spool's: losing the
//! record of a deletion is bad, and refusing to delete a page because the
//! record could not be written is worse. Callers log the failure and go on.

use anamnesis_core::audit::{Action, AuditEntry, Via};
use anamnesis_core::ids::ProjectId;
use anamnesis_core::scope::OperatorName;
use rusqlite::params;

use crate::Store;
use crate::convert::{parse_id, parse_time};

impl Store {
    /// Record one deliberate change to memory.
    pub fn append_audit(&self, entry: &AuditEntry) -> crate::Result<()> {
        let conn = self.connection();
        conn.execute(
            "INSERT INTO audit_log (id, at, project_id, operator, via, action, subject, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.id.to_string(),
                entry.at.to_string(),
                entry.project_id.map(|id| id.to_string()),
                entry.operator.as_ref().map(ToString::to_string),
                entry.via.as_str(),
                entry.action.as_str(),
                entry.subject,
                entry.detail,
            ],
        )?;
        Ok(())
    }

    /// The most recent changes, newest first.
    ///
    /// `project` narrows it to one project's memory; `None` reads the whole
    /// server, which is the question an operator of a shared one has.
    pub fn audit_trail(
        &self,
        project: Option<ProjectId>,
        limit: usize,
    ) -> crate::Result<Vec<AuditEntry>> {
        let conn = self.connection();
        let sql = "SELECT id, at, project_id, operator, via, action, subject, detail
                   FROM audit_log";
        let mut statement = match project {
            Some(_) => conn.prepare(&format!(
                "{sql} WHERE project_id = ?1 ORDER BY at DESC, rowid DESC LIMIT ?2"
            ))?,
            None => conn.prepare(&format!("{sql} ORDER BY at DESC, rowid DESC LIMIT ?1"))?,
        };

        let rows = match project {
            Some(project) => {
                statement.query_map(params![project.to_string(), limit as i64], read_entry)?
            }
            None => statement.query_map(params![limit as i64], read_entry)?,
        };
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// How many lines the log holds, for a report that says whether there is
    /// anything to read at all.
    pub fn audit_len(&self) -> crate::Result<i64> {
        let conn = self.connection();
        Ok(conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))?)
    }
}

/// One row, as it was written.
///
/// An action this build has no name for keeps the name it has: the entry is
/// still a real change, and relabelling it as something recognised is how a
/// log starts lying about itself. It is carried through as the subject's
/// prefix so a reader still sees it.
fn read_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntry> {
    let stored_action: String = row.get(5)?;
    let subject: String = row.get(6)?;
    let (action, subject) = match Action::from_storage(&stored_action) {
        Ok(action) => (action, subject),
        Err(unknown) => (Action::PageWritten, format!("[{unknown}] {subject}")),
    };

    Ok(AuditEntry {
        id: parse_id(row.get::<_, String>(0)?),
        at: parse_time(&row.get::<_, String>(1)?),
        project_id: row.get::<_, Option<String>>(2)?.map(parse_id),
        operator: row
            .get::<_, Option<String>>(3)?
            .and_then(|raw| OperatorName::sanitized(&raw).ok()),
        via: Via::from_storage(&row.get::<_, String>(4)?),
        action,
        subject,
        detail: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn store() -> Store {
        let store = Store::open_in_memory().expect("store");
        store.migrate().expect("migrate");
        store
    }

    fn at(raw: &str) -> Timestamp {
        raw.parse().expect("timestamp")
    }

    #[test]
    fn a_change_comes_back_out_as_it_went_in() {
        let store = store();
        let entry = AuditEntry::new(
            Action::PageForgotten,
            Via::Cli,
            "notes/api.md",
            at("2026-09-01T12:00:00Z"),
        )
        .by(Some(OperatorName::sanitized("alice").expect("operator")))
        .saying("1 page, commit ab12cd34");

        store.append_audit(&entry).expect("append");
        let trail = store.audit_trail(None, 10).expect("read");

        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0], entry);
    }

    /// Newest first, because the question is always "what just happened".
    #[test]
    fn the_most_recent_change_is_first() {
        let store = store();
        for (n, when) in [
            ("first", "2026-09-01T10:00:00Z"),
            ("second", "2026-09-01T11:00:00Z"),
            ("third", "2026-09-01T12:00:00Z"),
        ] {
            store
                .append_audit(&AuditEntry::new(Action::PageWritten, Via::Mcp, n, at(when)))
                .expect("append");
        }

        let trail = store.audit_trail(None, 10).expect("read");
        let subjects: Vec<&str> = trail.iter().map(|entry| entry.subject.as_str()).collect();

        assert_eq!(subjects, ["third", "second", "first"]);
    }

    /// A shared server holds several projects, and "what happened here" is a
    /// different question from "what happened at all".
    #[test]
    fn a_listing_can_be_narrowed_to_one_project() {
        let store = store();
        let mine = ProjectId::derive(
            &anamnesis_core::scope::WorkspaceName::default(),
            &anamnesis_core::scope::ProjectKey::from_name(
                &anamnesis_core::scope::ProjectName::sanitized("mine").expect("name"),
            ),
        );
        let theirs = ProjectId::derive(
            &anamnesis_core::scope::WorkspaceName::default(),
            &anamnesis_core::scope::ProjectKey::from_name(
                &anamnesis_core::scope::ProjectName::sanitized("theirs").expect("name"),
            ),
        );

        store
            .append_audit(
                &AuditEntry::new(
                    Action::PageWritten,
                    Via::Cli,
                    "ours.md",
                    at("2026-09-01T10:00:00Z"),
                )
                .in_project(mine),
            )
            .expect("append");
        store
            .append_audit(
                &AuditEntry::new(
                    Action::PageWritten,
                    Via::Cli,
                    "theirs.md",
                    at("2026-09-01T11:00:00Z"),
                )
                .in_project(theirs),
            )
            .expect("append");

        let trail = store.audit_trail(Some(mine), 10).expect("read");

        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].subject, "ours.md");
        assert_eq!(store.audit_len().expect("count"), 2);
    }

    /// The line that outlives what it describes. A foreign key that cascaded
    /// would delete the record of the deletion, which is the one line somebody
    /// goes looking for afterwards.
    #[test]
    fn a_line_survives_the_thing_it_is_about() {
        let store = store();
        let project = ProjectId::derive(
            &anamnesis_core::scope::WorkspaceName::default(),
            &anamnesis_core::scope::ProjectKey::from_name(
                &anamnesis_core::scope::ProjectName::sanitized("widget").expect("name"),
            ),
        );

        store
            .append_audit(
                &AuditEntry::new(
                    Action::PageForgotten,
                    Via::Cli,
                    "notes/gone.md",
                    at("2026-09-01T12:00:00Z"),
                )
                .in_project(project),
            )
            .expect("append");

        // Nothing was ever inserted for this project, so the audit row refers
        // to a project row that does not exist — which is exactly the state a
        // deletion leaves behind, and it has to be readable.
        let trail = store.audit_trail(Some(project), 10).expect("read");
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].subject, "notes/gone.md");
    }

    /// A log written by a newer build is still readable, and says so rather
    /// than quietly becoming something this build recognises.
    #[test]
    fn an_action_from_a_newer_build_is_carried_through_by_name() {
        let store = store();
        {
            let conn = store.connection();
            conn.execute(
                "INSERT INTO audit_log (id, at, project_id, operator, via, action, subject, detail)
                 VALUES (?1, ?2, NULL, NULL, 'cli', 'page.rewrapped', 'notes/api.md', NULL)",
                params![
                    anamnesis_core::ids::AuditId::new().to_string(),
                    at("2026-09-01T12:00:00Z").to_string()
                ],
            )
            .expect("insert");
        }

        let trail = store.audit_trail(None, 10).expect("read");

        assert_eq!(trail.len(), 1);
        assert!(
            trail[0].subject.contains("page.rewrapped"),
            "{}",
            trail[0].subject
        );
    }
}
