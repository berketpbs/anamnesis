-- Pages whose embedding was attempted and did not happen.
--
-- A failed embedding costs a page one retrieval stream and nothing else, which
-- is the right trade: refusing the write would cost the page. But the only
-- record of it was a `tracing::warn!` line, and a warning in a server's log is
-- not a record — it scrolls, it is absent from `doctor`, and nobody reads it on
-- the day it is written. So the failure mode was a page that is in the wiki, in
-- the index, in full-text and entity and link retrieval, and silently missing
-- from the one stream a person switched embedding on to get. Everything looks
-- healthy and the vector stream is quietly smaller than the corpus.
--
-- This is the same class of fault this project keeps writing tests against: not
-- that the system broke, but that the break was invisible from outside.
--
-- Keyed `(page_id, model)` to match `page_embeddings`, for the reason that
-- table gives: a page can hold a good vector under one model and have failed
-- under another, and those are different facts. A row here is the negative of a
-- row there — the two are mutually exclusive for a given key, and `embed_page`
-- is what keeps them so, deleting one as it writes the other.
--
-- ON DELETE CASCADE because a failure is a fact about a page, and a page that
-- is gone has no facts.
CREATE TABLE page_embed_failures (
    page_id TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    model   TEXT NOT NULL,
    -- When the most recent attempt failed. Overwritten on each retry rather
    -- than appended to: this table answers "what is missing a vector now", not
    -- "how often has this been tried", and an unbounded attempt log would make
    -- a persistently unreachable embedder look like a growing catastrophe.
    at      TEXT NOT NULL,
    -- The error as it was reported. Kept because the remedy differs completely
    -- between a model that would not load and a page that would not fit, and a
    -- count alone cannot tell those apart.
    reason  TEXT NOT NULL,
    PRIMARY KEY (page_id, model)
);

-- The question `doctor` asks: is anything in this project missing a vector?
CREATE INDEX idx_page_embed_failures_model ON page_embed_failures (model);
