-- Wiki pages: the index over markdown that remains the source of truth.
--
-- Every column here is rebuildable. Authored fields come from the file's
-- frontmatter; retention statistics are recomputed by observing use. Deleting
-- this database and reindexing the wiki loses no authored content — page
-- identifiers are derived from (project, path), so they come back identical.

CREATE TABLE pages (
    -- Derived UUIDv5 over (project_id, path).
    id               TEXT PRIMARY KEY,
    project_id       TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    path             TEXT NOT NULL,
    title            TEXT NOT NULL,
    body             TEXT NOT NULL,

    -- Temporal tier. One bounded ranking signal, never an absolute override.
    tier             TEXT NOT NULL
                     CHECK (tier IN ('working', 'episodic', 'semantic', 'procedural')),

    -- Trust level. `do-not-answer-from` stays retrievable on purpose: a claim
    -- known to be wrong is more useful visible than vanished, because it
    -- explains a contradiction the reader may already have in hand.
    status           TEXT NOT NULL
                     CHECK (status IN ('active', 'historical', 'do-not-answer-from', 'superseded')),

    -- First-order retention control: pinned pages skip the decay sweep
    -- entirely rather than being scored generously.
    pinned           INTEGER NOT NULL DEFAULT 0,
    canonical        INTEGER NOT NULL DEFAULT 0,

    -- Supersession chain. `supersedes` points at the page this one replaces;
    -- `is_latest` is the denormalised head-of-chain flag that retrieval filters
    -- on, derivable from the chain but stored so the common query stays cheap.
    supersedes       TEXT REFERENCES pages (id) ON DELETE SET NULL,
    is_latest        INTEGER NOT NULL DEFAULT 1,

    -- Decay inputs: salience is authored, the access statistics are observed.
    salience         REAL NOT NULL DEFAULT 1.0,
    access_count     INTEGER NOT NULL DEFAULT 0,
    last_accessed_at TEXT,

    expires_at       TEXT,
    git_commit       TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,

    UNIQUE (project_id, path)
);

CREATE INDEX idx_pages_project_tier ON pages (project_id, tier) WHERE is_latest = 1;
CREATE INDEX idx_pages_status ON pages (project_id, status);
CREATE INDEX idx_pages_supersedes ON pages (supersedes) WHERE supersedes IS NOT NULL;
CREATE INDEX idx_pages_updated ON pages (project_id, updated_at DESC);
-- The decay sweep scans only unpinned, expirable pages; keep that set narrow.
CREATE INDEX idx_pages_sweep ON pages (project_id, last_accessed_at) WHERE pinned = 0;
CREATE INDEX idx_pages_expiry ON pages (expires_at) WHERE expires_at IS NOT NULL;

-- Full-text search over page content, external-content against `pages`.
CREATE VIRTUAL TABLE pages_fts USING fts5 (
    path,
    title,
    body,
    content = 'pages',
    content_rowid = 'rowid',
    tokenize = "unicode61 remove_diacritics 2"
);

CREATE TRIGGER pages_fts_insert AFTER INSERT ON pages BEGIN
    INSERT INTO pages_fts (rowid, path, title, body)
    VALUES (new.rowid, new.path, new.title, new.body);
END;

CREATE TRIGGER pages_fts_delete AFTER DELETE ON pages BEGIN
    INSERT INTO pages_fts (pages_fts, rowid, path, title, body)
    VALUES ('delete', old.rowid, old.path, old.title, old.body);
END;

CREATE TRIGGER pages_fts_update AFTER UPDATE ON pages BEGIN
    INSERT INTO pages_fts (pages_fts, rowid, path, title, body)
    VALUES ('delete', old.rowid, old.path, old.title, old.body);
    INSERT INTO pages_fts (rowid, path, title, body)
    VALUES (new.rowid, new.path, new.title, new.body);
END;
