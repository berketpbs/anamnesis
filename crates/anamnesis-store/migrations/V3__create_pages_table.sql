-- Pages table for wiki content
CREATE TABLE IF NOT EXISTS pages (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    path TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    entities TEXT,
    git_commit TEXT,
    pinned INTEGER NOT NULL DEFAULT 0,
    expires_at TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(project_id, path)
);

CREATE INDEX idx_pages_project_id ON pages(project_id);
CREATE INDEX idx_pages_path ON pages(path);
CREATE INDEX idx_pages_title ON pages(title);
CREATE INDEX idx_pages_created_at ON pages(created_at);
CREATE INDEX idx_pages_updated_at ON pages(updated_at);
CREATE INDEX idx_pages_expires_at ON pages(expires_at);
CREATE INDEX idx_pages_pinned ON pages(pinned);
