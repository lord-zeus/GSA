-- F#31 — GSA "Comment text": the project-level comment body for the
-- comment / guestbook / blog-comment family of engines (the INIs fill the
-- form's message field with %blog_comment% / %guestbook_comment% /
-- %comment%). Spintax allowed. Empty = the runner auto-generates a comment
-- from the cloned comments.dat pool (F#22) — so a first usable backlink is
-- posted even before the user writes anything.
--
-- `comment_text` is additive: the column defaults to '' so every existing
-- project and the whole test suite are untouched.
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS comment_text TEXT NOT NULL DEFAULT '';
