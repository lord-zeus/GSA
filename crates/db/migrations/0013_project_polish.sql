-- F#26 Polish: per-project workflow options (GSA Notes tab + Pause-after-N).
--
-- notes          — GSA "Notes" free text (shown on the project card / export header).
-- color          — GSA "Custom Project Color" (a CSS hex like #4f9d5d; UI only).
-- pause_after    — GSA "Pause after N submissions": the pool soft-stops once a
--                  single run has submitted this many *successful* links
--                  (remaining pending jobs stay queued, resumable next run).
--                  0 / NULL = no pause (current behavior).
-- paused_at      — set by the pool the moment it hits the pause_after ceiling,
--                  so the UI can show "paused after N" and the run is resumable.
-- pause_after    / paused_at are additive — the columns default to no-pause so
--                  every existing project and the whole test suite are untouched.
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS notes       TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS color       TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS pause_after INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS paused_at   TIMESTAMPTZ;
