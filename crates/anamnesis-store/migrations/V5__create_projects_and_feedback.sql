-- Projects table for workspace/project tracking
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(workspace_id, name)
);

CREATE INDEX idx_projects_workspace_id ON projects(workspace_id);
CREATE INDEX idx_projects_name ON projects(name);

-- Feedback table for page usefulness ratings
CREATE TABLE IF NOT EXISTS page_feedback (
    id TEXT PRIMARY KEY,
    page_id TEXT NOT NULL,
    feedback_type TEXT NOT NULL,
    reason TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (page_id) REFERENCES pages(id) ON DELETE CASCADE
);

CREATE INDEX idx_page_feedback_page_id ON page_feedback(page_id);
CREATE INDEX idx_page_feedback_type ON page_feedback(feedback_type);
CREATE INDEX idx_page_feedback_created_at ON page_feedback(created_at);
