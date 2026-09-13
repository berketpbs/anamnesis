-- A page's abstract gets a vector of its own.
--
-- A page longer than the embedding model's window is embedded from its opening,
-- and since V17 also in sections; neither is one vector that stands for the
-- whole page. A page whose frontmatter carries `abstract:` — one line saying
-- what it is about — has that line embedded here, apart from its body, and the
-- line is short enough for every model to read whole.
--
-- A table of its own rather than another `part` of `page_embeddings`, because
-- it is a different stream rather than another piece of the same one: a query
-- ranks pages by their abstracts as a list of its own, and a page with no
-- abstract is absent from that list rather than compared by something else.
-- The same key as `page_embeddings` before sections, and for the same reason:
-- a new model name adds a row instead of replacing one still being compared.
CREATE TABLE page_abstract_embeddings (
    page_id TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    model   TEXT NOT NULL,
    dim     INTEGER NOT NULL,
    vector  BLOB NOT NULL,
    PRIMARY KEY (page_id, model)
);

CREATE INDEX idx_page_abstract_embeddings_model ON page_abstract_embeddings (model);
