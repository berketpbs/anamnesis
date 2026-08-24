-- Workstreams: named, persistent threads of work that can span many
-- sessions and many harnesses. See anamnesis_core::workstream for the
-- reasoning; this is the storage half of it.

CREATE TABLE workstreams (
    -- Derived UUIDv5 over (project_id, slug).
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    slug       TEXT NOT NULL,
    title      TEXT NOT NULL,
    status     TEXT NOT NULL CHECK (status IN ('active', 'paused', 'completed')) DEFAULT 'active',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (project_id, slug)
);

CREATE INDEX idx_workstreams_project_status ON workstreams (project_id, status);

-- A session may optionally join a workstream. Most sessions never do, and
-- that is the ordinary case, not a degraded one.
ALTER TABLE sessions ADD COLUMN workstream_id TEXT REFERENCES workstreams (id) ON DELETE SET NULL;
CREATE INDEX idx_sessions_workstream ON sessions (workstream_id) WHERE workstream_id IS NOT NULL;

-- Handoffs move from "one pending per project" to "one pending per
-- (project, workstream)". The old unique index enforced the former; it is
-- dropped and replaced rather than altered, because SQLite has no ALTER
-- INDEX. `COALESCE(workstream_id, '')` is what keeps every workstream-less
-- handoff collapsed onto one shared slot — the exact behaviour this index
-- already enforced before workstreams existed — while a real slug gets a
-- slot of its own. Plain NULL cannot do this: SQL treats every NULL as
-- distinct from every other NULL, so two workstream-less pending handoffs
-- would otherwise coexist instead of the second expiring the first.
ALTER TABLE handoffs ADD COLUMN workstream_id TEXT REFERENCES workstreams (id) ON DELETE SET NULL;
DROP INDEX idx_handoffs_single_pending;
CREATE UNIQUE INDEX idx_handoffs_single_pending
    ON handoffs (project_id, COALESCE(workstream_id, ''))
    WHERE state = 'pending';
