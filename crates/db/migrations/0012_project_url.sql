-- F#19 — the project's base target URL (GSA Data tab "URL (spintax)" field).
-- Every submission for this project is a backlink pointing AT this URL.
-- Optional so pre-0012 projects keep running; the runner treats an empty
-- URL as "engine-spec default" (previous behavior) until set.
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS url TEXT NOT NULL DEFAULT '';
