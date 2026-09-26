-- What a session that took over from another is owed after it started.
--
-- A handoff is claimed at a session's start, and the session it describes is
-- often still being written up then: the model's note lands about twenty
-- seconds after the terminal closes, its decisions with it, and a person who
-- switches agents at once has already been handed the counted note by then.
-- The model's note used to be dropped at that point, so that nobody after
-- would be handed a briefing on work already taken over. That was right for
-- them and wrong for the one session that took over: it had been handed the
-- lesser note, and was never handed the better one.
--
-- So it is kept here, addressed to that session alone, and delivered with its
-- next prompt. Not in `handoffs`: that row records what the session was
-- handed at its start, and it stays true to that.
CREATE TABLE handoff_followups (
    id           TEXT PRIMARY KEY,
    from_session TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    to_session   TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    -- The note written after the takeover, when one was. NULL when all that
    -- arrived was notes: a quiet session's decisions are written without one.
    body         TEXT,
    -- Durable pages written in the same pass, one path per line.
    notes        TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    delivered_at TEXT
);

CREATE INDEX idx_handoff_followups_waiting
    ON handoff_followups (to_session) WHERE delivered_at IS NULL;
