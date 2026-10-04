//! Find the most recent captured work in one handoff slot without claiming it.
use crate::{Result, Store};
use anamnesis_core::handoff::Slot;
use anamnesis_core::ids::{ProjectId, SessionId};
use rusqlite::{OptionalExtension, params};

impl Store {
    /// Latest session by captured activity within the exact project/workstream/operator slot.
    pub fn latest_session_for_slot(
        &self,
        project: ProjectId,
        slot: &Slot,
    ) -> Result<Option<SessionId>> {
        let conn = self.connection();
        let id: Option<String> = conn.query_row(
            "SELECT s.id FROM sessions s WHERE s.project_id = ?1
             AND coalesce(s.workstream_id, '') = coalesce(?2, '')
             AND (?3 IS NULL OR s.operator = ?3)
             ORDER BY coalesce((SELECT max(at) FROM observations o WHERE o.session_id = s.id), s.started_at) DESC, s.id DESC
             LIMIT 1",
            params![project.to_string(), slot.workstream_key(), slot.operator_key()],
            |row| row.get(0),
        ).optional()?;
        Ok(id.map(crate::convert::parse_id))
    }

    /// Last captured event of one kind, restricted to the given project and session.
    pub fn latest_event_of_kind(
        &self,
        project: ProjectId,
        session: SessionId,
        kind: anamnesis_core::observation::EventKind,
    ) -> Result<Option<anamnesis_core::observation::Observation>> {
        let conn = self.connection();
        Ok(conn
            .query_row(
                "SELECT o.id, o.session_id, o.kind, o.tool_name, o.tool_ok, o.at, o.body,
                    o.truncated, o.sanitized, o.tool_call_id
             FROM observations o JOIN sessions s ON s.id = o.session_id
             WHERE s.project_id = ?1 AND s.id = ?2 AND o.kind = ?3
             ORDER BY o.at DESC, o.rowid DESC LIMIT 1",
                params![project.to_string(), session.to_string(), kind.as_str()],
                crate::ops::read_observation,
            )
            .optional()?)
    }
}
