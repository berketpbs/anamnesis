-- Improvement proposals: what a pass noticed, and what it wants done.
--
-- A proposal is an observation of a condition, not a task queue entry. Its
-- identifier is derived from `(project, kind, subject)`, so a later pass that
-- notices the same thing arrives at the same row rather than filing a second
-- copy — which is what makes a decision stick. Dismiss a proposal and it stays
-- dismissed, because the next pass cannot help but land on the row that says so.

CREATE TABLE proposals (
    -- Derived UUIDv5 over (project_id, kind, subject).
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,

    kind       TEXT NOT NULL
               CHECK (kind IN ('promote-tier', 'write-missing-page')),

    -- What the proposal is about: a page path, or a link target no page
    -- answers to. Unique per kind within a project, which the derived id
    -- already implies and this constraint enforces against anything else.
    subject    TEXT NOT NULL,

    -- Set when the subject is a page that exists. A proposal about a page that
    -- is later swept goes with it: there is nothing left to promote.
    page_id    TEXT REFERENCES pages (id) ON DELETE CASCADE,

    -- The evidence, in the words the report uses. Refreshed while a proposal
    -- is open, because a page read twelve times is a different argument from
    -- the same page read three times.
    rationale  TEXT NOT NULL,

    -- `resolved` is not a decision anyone made: it means the condition stopped
    -- holding, usually because a person did the thing themselves.
    state      TEXT NOT NULL
               CHECK (state IN ('open', 'applied', 'dismissed', 'resolved')),

    created_at TEXT NOT NULL,
    decided_at TEXT,

    UNIQUE (project_id, kind, subject)
);

CREATE INDEX idx_proposals_open
    ON proposals (project_id, kind, subject) WHERE state = 'open';

-- Where the project's working copy is, so a scheduler running inside one
-- server can find each project's marker file and read the settings that
-- govern it. Nullable: rows written before this column existed do not know,
-- and a project registered from a directory that has since moved is wrong
-- rather than absent — both are treated as "cannot be scheduled", never
-- guessed at.
ALTER TABLE projects ADD COLUMN root_path TEXT;

-- When an improvement pass last ran for this project, so an interval means
-- "since the last pass" rather than "since the server started".
ALTER TABLE projects ADD COLUMN improved_at TEXT;
