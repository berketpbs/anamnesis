-- The supersession a page *authored*, beside the one that resolved.
--
-- `supersedes` is a foreign key, so it can only ever hold a page that exists.
-- That is not enough on its own: a page can name its predecessor before the
-- index has seen it — during a rebuild that visits paths in an order nobody
-- chose, or when a page is written by hand — and a pointer that could not be
-- resolved at the moment of writing has to be resolvable later. Keeping the
-- authored path is what makes that possible, exactly as `page_links.to_target`
-- keeps the link someone wrote beside the page it resolved to.
--
-- Nullable, and NULL for every row written before this column existed: those
-- rows never had their supersession recorded at all, and `anamnesis reindex`
-- is what puts it back from the markdown that always had it.
ALTER TABLE pages ADD COLUMN supersedes_target TEXT;

-- Resolving a supersession asks "which page names this path?", which is a scan
-- of one project's rows without this.
CREATE INDEX idx_pages_supersedes_target
    ON pages (project_id, supersedes_target) WHERE supersedes_target IS NOT NULL;
