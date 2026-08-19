-- Projects, sessions, and the observations captured within them.
--
-- Timestamps are RFC 3339 with an explicit `Z` (`2026-08-19T16:04:21Z`), always
-- written from Rust. SQLite's own CURRENT_TIMESTAMP produces a different shape
-- (`2026-08-19 16:04:21` — no `T`, no offset), and mixing the two would break
-- both `jiff` parsing and the lexicographic ordering these indexes rely on, so
-- no column here carries a SQL default.

CREATE TABLE projects (
    -- Derived UUIDv5, reproducible from workspace + project_key.
    id            TEXT PRIMARY KEY,
    workspace_id  TEXT NOT NULL,
    workspace     TEXT NOT NULL,
    name          TEXT NOT NULL,
    -- The canonical string `id` was derived from, e.g. `git:github.com/acme/api`.
    project_key   TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE (workspace, name),
    UNIQUE (workspace_id, project_key)
);

CREATE INDEX idx_projects_workspace ON projects (workspace_id);

-- Sessions reference the project only. Workspace is reachable through it, so
-- there is no way to record a session whose workspace and project disagree —
-- a pairing that has to be repaired later if both are stored side by side.
CREATE TABLE sessions (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- Free text, validated in Rust. Deliberately not a CHECK constraint:
    -- supporting a new harness must not require a schema migration.
    agent         TEXT NOT NULL,
    checkout_path TEXT NOT NULL,
    state         TEXT NOT NULL CHECK (state IN ('open', 'ending', 'closed')),
    started_at    TEXT NOT NULL,
    ended_at      TEXT
);

CREATE INDEX idx_sessions_project_started ON sessions (project_id, started_at DESC);
CREATE INDEX idx_sessions_state ON sessions (state) WHERE state != 'closed';

CREATE TABLE observations (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    tool_name  TEXT,
    tool_ok    INTEGER,
    at         TEXT NOT NULL,
    body       TEXT NOT NULL,
    -- Whether the body was cut to fit its byte budget.
    truncated  INTEGER NOT NULL DEFAULT 0,
    -- Whether redaction has been applied.
    sanitized  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_observations_session_at ON observations (session_id, at);
CREATE INDEX idx_observations_kind ON observations (kind);
CREATE INDEX idx_observations_tool ON observations (tool_name) WHERE tool_name IS NOT NULL;
