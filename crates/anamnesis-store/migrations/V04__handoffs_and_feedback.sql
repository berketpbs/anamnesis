-- Handoffs between sessions, and the feedback signals that tune retention.

CREATE TABLE handoffs (
    id           TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    from_session TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    to_session   TEXT REFERENCES sessions (id) ON DELETE SET NULL,
    body         TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'expired')),
    created_at   TEXT NOT NULL,
    accepted_at  TEXT
);

-- A handoff is single-use, and this is where that is actually enforced: at most
-- one pending handoff can exist per project, so two agents starting at the same
-- moment cannot both be handed the same "here is where I left off" note.
-- Superseding an unread handoff means expiring it first.
CREATE UNIQUE INDEX idx_handoffs_single_pending
    ON handoffs (project_id) WHERE state = 'pending';

CREATE INDEX idx_handoffs_from ON handoffs (from_session);

-- Feedback adjusts retention and surfaces contradictions. `stale` and `wrong`
-- are not deletions: they lower a page's standing and mark it for review, which
-- is what keeps a disagreement visible instead of silently resolved.
CREATE TABLE page_feedback (
    id         TEXT PRIMARY KEY,
    page_id    TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    signal     TEXT NOT NULL
               CHECK (signal IN ('helpful', 'not-helpful', 'stale', 'wrong')),
    note       TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_page_feedback_page ON page_feedback (page_id, created_at DESC);
CREATE INDEX idx_page_feedback_signal ON page_feedback (signal);
