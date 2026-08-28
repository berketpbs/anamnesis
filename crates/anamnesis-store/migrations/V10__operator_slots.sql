-- Per-operator handoff slots, for a server more than one person uses.
--
-- A handoff is single-use, and until now "used" meant used by whoever started
-- next. On one person's machine that is exactly right. On a shared server it
-- is a silent theft: the note one operator's session wrote for their own next
-- session is handed to whoever happens to start first, and neither of them
-- can tell it happened — the first reads someone else's context as their own,
-- the second finds nothing waiting.
--
-- Who an operator is comes from the bearer token they present; see
-- `anamnesis_web::auth`. A caller the server cannot tell apart from any other
-- has no operator, and shares the slot every anonymous caller shares, which is
-- the behaviour every single-user install already has.

-- Recorded on every session, whether or not the project keys slots by it.
-- "Whose session was this" is a fact worth having in its own right, and
-- storing it only when `[slots] per_user` is on would mean turning the setting
-- on could not explain anything about the sessions that came before.
ALTER TABLE sessions ADD COLUMN operator TEXT;
CREATE INDEX idx_sessions_operator ON sessions (operator) WHERE operator IS NOT NULL;

-- On the handoff, this is the slot key rather than a record of provenance:
-- it is NULL unless the project asked for per-operator slots, so a project
-- that has not asked keeps exactly one pending handoff as before.
ALTER TABLE handoffs ADD COLUMN operator TEXT;

-- The same `COALESCE` shape V06 used for workstreams, and for the same reason:
-- SQL treats every NULL as distinct from every other NULL, so a plain column
-- in a unique index would let two slot-less pending handoffs coexist instead
-- of the second expiring the first. Dropped and recreated because SQLite has
-- no ALTER INDEX.
DROP INDEX idx_handoffs_single_pending;
CREATE UNIQUE INDEX idx_handoffs_single_pending
    ON handoffs (project_id, COALESCE(workstream_id, ''), COALESCE(operator, ''))
    WHERE state = 'pending';
