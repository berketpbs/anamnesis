-- Where what a page says came from: `human`, `agent` or `repo`.
--
-- Authored in the page's frontmatter, which is its source of truth; kept here
-- so that what a starting session and recall are shown can say it without
-- reading every file. NULL is unknown, which is every page from before this
-- and every page written by hand or from the command line.
ALTER TABLE pages ADD COLUMN origin TEXT;
