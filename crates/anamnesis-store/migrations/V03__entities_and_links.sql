-- The two retrieval streams that sit beside full-text search: entity matching
-- and graph-neighbour expansion.

-- Canonical names, deduplicated per project so an inverse-frequency weight can
-- be computed: an entity naming half the wiki carries less signal than one
-- naming three pages.
CREATE TABLE entities (
    id         INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    UNIQUE (project_id, name)
);

CREATE TABLE page_entities (
    page_id   TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    entity_id INTEGER NOT NULL REFERENCES entities (id) ON DELETE CASCADE,
    PRIMARY KEY (page_id, entity_id)
);

-- Reverse lookup: every page mentioning an entity.
CREATE INDEX idx_page_entities_entity ON page_entities (entity_id, page_id);

-- Wiki links. A link is recorded as written, whether or not it resolves:
-- `[[some-page]]` pointing at a page that does not exist yet is a marker of
-- intent, not an error, and it must resolve by itself once that page is
-- created. `to_page_id` stays NULL until then.
CREATE TABLE page_links (
    from_page_id  TEXT NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    -- Link target exactly as authored.
    to_target     TEXT NOT NULL,
    to_page_id    TEXT REFERENCES pages (id) ON DELETE SET NULL,
    -- Set when the link crosses into another project.
    to_project_id TEXT REFERENCES projects (id) ON DELETE CASCADE,
    PRIMARY KEY (from_page_id, to_target)
);

CREATE INDEX idx_page_links_to ON page_links (to_page_id) WHERE to_page_id IS NOT NULL;
CREATE INDEX idx_page_links_unresolved ON page_links (to_target) WHERE to_page_id IS NULL;
