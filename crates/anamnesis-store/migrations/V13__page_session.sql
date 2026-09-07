-- Which session's consolidation wrote a page.
--
-- Nothing could answer that. The only session-to-page mapping in the system was
-- the derived path `sessions/<date>-<short id>.md`, which works while a session
-- produces exactly one page and stops working the moment it produces several:
-- a later run that names its pages differently leaves the earlier ones on disk,
-- in the index, and in every retrieval stream, with nothing able to find them
-- and say they are stale.
--
-- The column is an index of the frontmatter, not the record of it. A page
-- carries `session:` in its own markdown, because this database is rebuilt from
-- the wiki and a fact only the database held would not survive `reindex`.
--
-- NULL is "not written by a session" — a page somebody wrote by hand, a
-- bootstrap page, or any page that existed before this migration. It is never
-- "session unknown": nothing here guesses, and a sweep that treated an unknown
-- as an orphan would delete the pages people wrote themselves.
--
-- ON DELETE SET NULL rather than CASCADE, and the difference matters:
-- `forget-session` removes a session and its observations, and the page it
-- produced is a separate decision with its own command. A cascade here would
-- make forgetting a transcript silently delete the durable knowledge compiled
-- from it, which is the opposite of what this system is for.
ALTER TABLE pages ADD COLUMN session_id TEXT
    REFERENCES sessions (id) ON DELETE SET NULL;

-- The question this exists to answer: what did this session write?
CREATE INDEX idx_pages_session ON pages (session_id) WHERE session_id IS NOT NULL;
