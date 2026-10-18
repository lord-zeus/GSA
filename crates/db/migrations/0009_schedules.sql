-- F#17: Per-project scheduled runs (GSA "scheduler" / scheduled posting).
--
-- One new table on the existing schema. A schedule row is a
-- self-contained, pure, declarative calendar: given `start_at`,
-- `mode`, `repeat_every_minutes`, `repeat_weekdays`, `window_start/end`,
-- `stop_at`, and optionally `max_runs`, the API can compute the next
-- eligible fire time deterministically.
--
-- `next_run_at` is the **last fired** slot (NULL = never fired / newly
-- created). After each fire the API claims `next_run_at` atomically:
-- the row is only claimed if `next_run_at < now` (a slot that's already
-- passed) and the row hasn't been retired — see
-- `Repo::claim_schedule` for the exact invariant.
--
-- `run_count` is incremented on every fire; `enabled` toggles whether the
-- scheduler considers the row.

CREATE TABLE IF NOT EXISTS schedules (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id          UUID NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name                TEXT NOT NULL,
    mode                TEXT NOT NULL CHECK (mode IN ('once', 'recurring')),
    start_at            TIMESTAMPTZ NOT NULL,
    stop_at             TIMESTAMPTZ,
    repeat_every_minutes INTEGER CHECK (repeat_every_minutes IS NULL OR (repeat_every_minutes BETWEEN 1 AND 525600)),
    repeat_weekdays     INTEGER[],          -- ISO 1=Mon..7=Sun, NULL = any
    window_start        TIME,               -- wall-clock window start (UTC)
    window_end          TIME,               -- wall-clock window end (UTC)
    max_runs            BIGINT CHECK (max_runs IS NULL OR max_runs > 0),
    run_count           INTEGER NOT NULL DEFAULT 0,
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    next_run_at         TIMESTAMPTZ,        -- last-fired slot (NULL = none)
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Unique per (project, start_at) to prevent two identical schedules on
    -- the same project at the same anchor (typo protection).
    UNIQUE (project_id, mode, start_at)
);

-- The scheduler query shape: WHERE enabled = TRUE ORDER BY next_run_at
-- (NULLS FIRST so "never fired" sorts before "fire in the future").
CREATE INDEX IF NOT EXISTS idx_schedules_enabled_next
    ON schedules (enabled, next_run_at NULLS FIRST)
    WHERE enabled = TRUE;

-- Per-project listing (the UI lists a project's schedules).
CREATE INDEX IF NOT EXISTS idx_schedules_project
    ON schedules (project_id, name);
