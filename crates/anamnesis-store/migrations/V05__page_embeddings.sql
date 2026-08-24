-- Page embeddings: the fourth retrieval stream, vector-cosine similarity.
--
-- One row per page, holding the vector produced by whichever local model
-- embedded it last. `model` is part of the key rather than assumed constant
-- because switching models mid-project must not silently compare vectors from
-- two different embedding spaces — a page not yet re-embedded under a new
-- model is simply absent from that model's similarity search, not wrongly
-- scored.
CREATE TABLE page_embeddings (
    page_id TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    model   TEXT NOT NULL,
    dim     INTEGER NOT NULL,
    -- Little-endian f32 vector, L2-normalized at write time so similarity is a
    -- plain dot product rather than a divide on every comparison.
    vector  BLOB NOT NULL,
    PRIMARY KEY (page_id, model)
);

CREATE INDEX idx_page_embeddings_model ON page_embeddings (model);
