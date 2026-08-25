-- The tokens an entity name is made of.
--
-- `entities.name` is what someone wrote — `Windows BOM`, `anamnesis-llm`,
-- `lib.rs`. A query is tokenized before it is matched, so a name kept whole
-- can only ever be found by a query that happens to tokenize to exactly it,
-- which for anything containing a space, a dash, or a dot is never. The page
-- was reachable through full text and through nothing else, and no failure
-- was reported anywhere.
--
-- Splitting the name once, at write time, is what lets the match ask the
-- question it means: are all of the tokens in this name present in the query?
CREATE TABLE entity_tokens (
    entity_id INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    token     TEXT NOT NULL,
    PRIMARY KEY (entity_id, token)
);

-- The direction the match runs in: token in hand, which entities use it.
CREATE INDEX idx_entity_tokens_token ON entity_tokens (token, entity_id);
