-- T6 — Live run visibility + engine health.
--
-- 1) job_events: append-only per-project log of what the runner does —
--    engine selection, each HTTP step (method/url/status), captcha attempts,
--    submit success/failure. The UI tails this table (poll) and the live SSE
--    stream (push) so a run is never a blank table again.
--
-- 2) Per-engine health counters on the engines table (GSA "engine history"):
--    attempts / successes / captcha_failures + last timestamps + last error.
--    Written by the runner after every attempt (auto-run and test-engine).
--    Used to RANK engine auto-selection: best-proven engines first, captcha
--    walls demoted.

CREATE TABLE IF NOT EXISTS job_events (
    id            BIGSERIAL PRIMARY KEY,
    project_id    UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    job_id        UUID REFERENCES jobs(id) ON DELETE SET NULL,
    engine_name   TEXT,
    level         TEXT NOT NULL DEFAULT 'info'
                  CHECK (level IN ('info','engine','step','captcha','success','error','auto')),
    message       TEXT NOT NULL,
    ts            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_job_events_project ON job_events(project_id, id);

ALTER TABLE engines ADD COLUMN IF NOT EXISTS attempts         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE engines ADD COLUMN IF NOT EXISTS successes        INTEGER NOT NULL DEFAULT 0;
ALTER TABLE engines ADD COLUMN IF NOT EXISTS captcha_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE engines ADD COLUMN IF NOT EXISTS last_attempt_at  TIMESTAMPTZ;
ALTER TABLE engines ADD COLUMN IF NOT EXISTS last_success_at  TIMESTAMPTZ;
ALTER TABLE engines ADD COLUMN IF NOT EXISTS last_error       TEXT;
