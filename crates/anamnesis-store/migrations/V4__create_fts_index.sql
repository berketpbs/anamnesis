-- Full-text search index for pages using FTS5
CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
    path,
    title,
    body,
    content=pages,
    content_rowid=rowid
);

-- Triggers to keep FTS5 index in sync with pages table
CREATE TRIGGER pages_ai AFTER INSERT ON pages BEGIN
  INSERT INTO pages_fts(rowid, path, title, body)
  VALUES (new.rowid, new.path, new.title, new.body);
END;

CREATE TRIGGER pages_ad AFTER DELETE ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, path, title, body)
  VALUES('delete', old.rowid, old.path, old.title, old.body);
END;

CREATE TRIGGER pages_au AFTER UPDATE ON pages BEGIN
  INSERT INTO pages_fts(pages_fts, rowid, path, title, body)
  VALUES('delete', old.rowid, old.path, old.title, old.body);
  INSERT INTO pages_fts(rowid, path, title, body)
  VALUES (new.rowid, new.path, new.title, new.body);
END;
