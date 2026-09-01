-- Who changed memory, and what they changed.
--
-- Capture already records what happened inside sessions. This answers the
-- other question, and one nothing could answer until now: who reached in and
-- changed the memory itself. A page rewritten by hand, a session forgotten, a
-- handoff claimed, a proposal applied — each replaces or removes something a
-- later session would otherwise have been told, and without a line saying so
-- the only evidence is that memory now says something different.
--
-- It is the precondition for pointing more than one person at one server. Not
-- because anyone is expected to misuse it, but because a shared memory whose
-- changes cannot be traced is one nobody can trust an answer from: "why does
-- this page say that now" has to have an answer that is not a guess.

CREATE TABLE audit_log (
    id         TEXT PRIMARY KEY,
    at         TEXT NOT NULL,

    -- Deliberately NOT a foreign key, and this is the whole design of the
    -- table. An audit line about a page that has since been forgotten is
    -- exactly the line somebody needs afterwards; a reference that cascaded
    -- would delete the record of the deletion. The project id is here to scope
    -- a listing, and it is text because the row outlives the row it names.
    project_id TEXT,

    -- The operator a bearer token named, when one did. NULL on every
    -- single-user install, where stamping "unknown" on every line would put
    -- noise where a fact nobody was looking for would go.
    operator   TEXT,

    -- Which door the change came through: cli, mcp, http, server. The same
    -- action means different things through different ones — a page written
    -- over MCP was written by a model mid-session, the same page written from
    -- the CLI was written by a person who meant to.
    via        TEXT NOT NULL,

    -- What was done, as `page.written`, `session.forgotten`, and so on. Not
    -- constrained by a CHECK: a build newer than this schema will write
    -- actions this one has no name for, and refusing them would mean the log
    -- loses exactly the lines somebody upgraded to get.
    action     TEXT NOT NULL,

    -- What it was done to: a page path, a session id, a proposal.
    subject    TEXT NOT NULL,

    -- Anything else worth keeping, in a person's words.
    detail     TEXT
);

-- The two ways it is read: everything newest-first, and one project's history.
CREATE INDEX idx_audit_at ON audit_log (at DESC);
CREATE INDEX idx_audit_project ON audit_log (project_id, at DESC);
