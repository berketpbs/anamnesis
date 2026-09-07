-- What actually wrote each session's page: a model, or the deterministic count.
--
-- `status` could only ever report the model the server was *configured* with,
-- and that is a different claim from the one anyone reading it makes. A server
-- pointed at a model that answers every request with 503 keeps accepting
-- sessions, keeps reporting that model's name, and writes a tally for every
-- one of them. Nothing in the system said so: capture works identically either
-- way, `/health` answers `ok`, and the only trace was a warning in a log file
-- nobody opens. This column is what lets the difference be seen from outside.
--
-- NULL means the session has not been summarised yet, which is the honest
-- state for every row that existed before this migration: the provenance of
-- those pages is not recoverable, and guessing 'model' for them would be
-- inventing the very fact this column exists to establish.
ALTER TABLE sessions ADD COLUMN summary_source TEXT
    CHECK (summary_source IN ('model', 'counted'));

-- The model that wrote the page, or — when `summary_source` is 'counted' — the
-- model that was configured and did not answer.
--
-- Both cases are worth keeping, because they are different faults with
-- different fixes: 'counted' with a model named here means the provider failed
-- and the thing to do is look at why, while 'counted' with NULL means no model
-- was ever configured and the thing to do is configure one.
ALTER TABLE sessions ADD COLUMN summary_model TEXT;

-- Answering "what has this server been writing lately?" is a scan of the most
-- recent sessions, which is how `status` asks it.
CREATE INDEX idx_sessions_summary ON sessions (project_id, started_at DESC)
    WHERE summary_source IS NOT NULL;
