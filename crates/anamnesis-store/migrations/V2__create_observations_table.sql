-- Observations table for session lifecycle events
CREATE TABLE IF NOT EXISTS observations (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    tool_name TEXT,
    timestamp TEXT NOT NULL,
    payload TEXT NOT NULL,
    sanitized INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX idx_observations_session_id ON observations(session_id);
CREATE INDEX idx_observations_event_type ON observations(event_type);
CREATE INDEX idx_observations_timestamp ON observations(timestamp);
CREATE INDEX idx_observations_tool_name ON observations(tool_name);
