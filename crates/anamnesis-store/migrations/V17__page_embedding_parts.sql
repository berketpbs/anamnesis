-- A page's vector can come in parts.
--
-- The default embedding model reads a page to its first 128 tokens and returns
-- an ordinary vector for however much that was. On this project's own wiki,
-- 43 of 49 pages were longer than that, so the one vector a page had stood for
-- its opening and nothing after it. The page is now also embedded in sections
-- the model reads whole, and a section needs a row of its own.
--
-- `part` 0 is the vector every page has always had — the page as one text, read
-- as far as the model reads — and every row that exists today becomes part 0,
-- unchanged. Parts 1 and up are sections, written only for a page too long for
-- one read. Keeping part 0 for those pages as well is what lets a query be
-- asked both ways over the same index, so a change to how pages are matched can
-- be measured against what it replaces rather than argued for.
--
-- SQLite cannot change a primary key in place, so the table is rebuilt.
CREATE TABLE page_embeddings_parts (
    page_id TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    model   TEXT NOT NULL,
    part    INTEGER NOT NULL DEFAULT 0 CHECK (part >= 0),
    dim     INTEGER NOT NULL,
    vector  BLOB NOT NULL,
    PRIMARY KEY (page_id, model, part)
);

INSERT INTO page_embeddings_parts (page_id, model, part, dim, vector)
SELECT page_id, model, 0, dim, vector FROM page_embeddings;

DROP TABLE page_embeddings;

ALTER TABLE page_embeddings_parts RENAME TO page_embeddings;

CREATE INDEX idx_page_embeddings_model ON page_embeddings (model);
