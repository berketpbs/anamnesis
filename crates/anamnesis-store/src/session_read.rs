//! Bounded, scoped access to captured source events, without reconstruction.

use anamnesis_core::ids::{ProjectId, SessionId};
use anamnesis_core::observation::EventKind;
use anamnesis_core::sanitize::Redactor;
use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

/// An event exactly as captured, with today's redaction applied on read.
#[derive(Debug, serde::Serialize)]
pub struct StoredEvent {
    /// Stable observation identifier.
    pub id: String,
    /// Captured timestamp.
    pub at: String,
    /// Lifecycle event type.
    pub kind: String,
    /// Stored text; never synthesized from a summary.
    pub text: String,
    /// Whether capture cut off part of this event's text.
    pub truncated: bool,
    /// Whether current rules masked additional text during this read.
    pub redacted_on_read: bool,
}

/// One bounded page of a project's captured session.
#[derive(Debug, serde::Serialize)]
pub struct SessionEvents {
    /// Full session identifier.
    pub session_id: String,
    /// Capturing client.
    pub agent: String,
    /// Number of events matching the requested type.
    pub matching_events: usize,
    /// Offset within the filtered, chronological event sequence.
    pub offset: usize,
    /// Next offset, or none when this sequence ends.
    pub next_offset: Option<usize>,
    /// Whether any source events remain in storage, independent of filtering.
    pub source_available: bool,
    /// Whether at least one user message was captured.
    pub user_messages_available: bool,
    /// Whether at least one assistant message was captured.
    pub assistant_messages_available: bool,
    /// Captured events, oldest first.
    pub events: Vec<StoredEvent>,
}

impl Store {
    /// Read at most 100 stored events, applying current redaction and project scope.
    ///
    /// Missing sessions and sessions in another project both return `None`.
    /// Filters and offsets apply before limiting. No wiki summary or client
    /// archive is used to fill missing messages. The index is not modified.
    pub fn read_session_events(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
        kind: Option<EventKind>,
        offset: usize,
        limit: usize,
    ) -> Result<Option<SessionEvents>> {
        let conn = self.connection();
        let identity = session_id.to_string();
        let agent: Option<String> = conn
            .query_row(
                "SELECT agent FROM sessions WHERE id = ?1 AND project_id = ?2",
                params![identity, project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(agent) = agent else {
            return Ok(None);
        };
        let (total, users, assistants): (usize, usize, usize) = conn.query_row(
            "SELECT count(*), coalesce(sum(kind = 'user-prompt'), 0),
                    coalesce(sum(kind = 'assistant-message'), 0)
             FROM observations WHERE session_id = ?1",
            [&identity],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let kind = kind.map(|kind| kind.as_str());
        let matching_events = conn.query_row(
            "SELECT count(*) FROM observations WHERE session_id = ?1 AND (?2 IS NULL OR kind = ?2)",
            params![identity, kind],
            |row| row.get(0),
        )?;
        let limit = limit.clamp(1, 100);
        let mut stmt = conn.prepare(
            "SELECT id, at, kind, body, truncated FROM observations
             WHERE session_id = ?1 AND (?2 IS NULL OR kind = ?2)
             ORDER BY at, rowid LIMIT ?3 OFFSET ?4",
        )?;
        let redactor = Redactor::new();
        let rows = stmt.query_map(
            params![
                identity,
                kind,
                (limit + 1) as i64,
                i64::try_from(offset).unwrap_or(i64::MAX)
            ],
            |row| {
                let stored: String = row.get(3)?;
                let text = redactor.redact(&stored).text().to_owned();
                Ok(StoredEvent {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    kind: row.get(2)?,
                    redacted_on_read: text != stored,
                    text,
                    truncated: row.get(4)?,
                })
            },
        )?;
        let mut events = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let next_offset = if events.len() > limit {
            events.pop();
            offset.checked_add(limit)
        } else {
            None
        };
        Ok(Some(SessionEvents {
            session_id: identity,
            agent,
            matching_events,
            offset,
            next_offset,
            source_available: total > 0,
            user_messages_available: users > 0,
            assistant_messages_available: assistants > 0,
            events,
        }))
    }
}
