-- What kind of wrong a page's vector is.
--
-- V15 made `page_embed_failures` the strict negative of `page_embeddings`: a
-- page had a vector or a complaint, never both, and `embed_page` kept it so by
-- deleting one as it wrote the other. That invariant was right for the fault it
-- was built for and it cannot hold for the one found next.
--
-- A model with a fixed context does not refuse a long page. It embeds the part
-- it can reach and returns an ordinary vector. For the default embedder that
-- window is 512 tokens, and nothing about the resulting vector says it stands
-- for a third of a gotcha. So the page has a vector *and* a complaint, and both
-- are true at once.
--
-- Worse, the two providers disagree about the same page. The hosted embedder
-- sends the text whole, the endpoint answers 400, and that already lands here
-- as a row. The local one silently halves it. Switching provider therefore
-- changes whether an over-long page is reported at all — which is exactly the
-- class of invisibility this table was added to end, arriving through the door
-- the table left open.
--
-- So the invariant widens rather than breaks, and is still one sentence: a row
-- here means this page's vector is missing or incomplete, and `kind` says
-- which. `failed` keeps its old meaning exactly.
--
-- DEFAULT 'failed' so that every row V15 wrote keeps saying what it said.
-- CHECK rather than a lookup table, because two values that are named in a
-- migration comment are cheaper to read here than to join.
ALTER TABLE page_embed_failures
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'failed'
    CHECK (kind IN ('failed', 'truncated'));

-- How many tokens the text came to, and how many the model could read. Null on
-- every `failed` row: a page that never reached the model has no length the
-- model would recognise, and writing a guess there would be a number somebody
-- later quotes.
ALTER TABLE page_embed_failures ADD COLUMN tokens INTEGER;
ALTER TABLE page_embed_failures ADD COLUMN budget INTEGER;

-- `doctor` asks for one project's rows and splits them by kind; the model index
-- from V15 does not serve that, and a scan of a table this size would, but the
-- ordering is what makes the report readable rather than the filtering.
CREATE INDEX idx_page_embed_failures_kind ON page_embed_failures (kind);
