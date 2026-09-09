-- The harness's own identifier for a tool call.
--
-- What ties an attempt to its completion. A harness fires one hook before a
-- tool runs and another after it, and until now both were recorded as the same
-- kind with nothing to pair them by — so a call that ran twice and a call that
-- was attempted once and never came back looked alike.
--
-- The pairing is what makes a failure visible on the harness this project runs
-- on. Claude Code fires no post-tool hook for a call that failed, so the
-- failure is not an unflagged event in the record; it is absent from it. An
-- attempt whose identifier never appears again is that absence, and it is the
-- only evidence of it there is.
--
-- NULL is the ordinary case and always will be: most harnesses send no such
-- identifier, and every observation written before this migration has none. It
-- means "cannot be paired", never "did not complete" — consolidation falls back
-- to counting when the identifiers are missing, because inventing a failure out
-- of a field a harness never sends would be worse than reporting none.
ALTER TABLE observations ADD COLUMN tool_call_id TEXT;

-- The question this exists to answer, asked once per session at consolidation:
-- which of this session's attempts never reported back?
CREATE INDEX idx_observations_tool_call
    ON observations (session_id, tool_call_id)
    WHERE tool_call_id IS NOT NULL;
