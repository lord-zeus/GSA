-- F#30 — GSA "Article content": the project-level article body for the
-- article-family engines (Rentry, Web2.0, blog, forums with post body…).
-- Spintax allowed. Empty = the runner auto-generates a short article
-- (title + a couple of comments + the target URL anchor sentence) from the
-- project keyword — enough for a first usable backlink, editable in the UI.
--
-- `article_text` is additive: the column defaults to '' so every existing
-- project and the whole test suite are untouched.
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS article_text TEXT NOT NULL DEFAULT '';
