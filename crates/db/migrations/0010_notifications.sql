-- F#18: Notification log + alert-channel configuration.
--
-- One table for the *record* of every alert the user should see (the DB
-- is the source of truth for the "Notification Center" UI — the SSE
-- stream is only the live pipe; a page the user opens after the fact
-- always queries this table). The `type` column mirrors
-- `engine::notify::NotificationType` (camelCase strings) and
-- `severity` mirrors `engine::notify::Severity` (lowercase strings), so
-- the API's `Notifier` can write either shape without a conversion step.
--
-- Channel configuration (SMTP host/port/user/password/recipient, and the
-- per-type `email_enabled` toggle) lives in the existing `settings`
-- table under a single JSON key `notify_config`; the API's
-- `GET/PUT /api/settings/notify` reads/writes it.

CREATE TABLE IF NOT EXISTS notifications (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- camelCase enum from engine::notify::NotificationType::as_str().
    type          TEXT    NOT NULL,
    -- lowercase enum from engine::notify::Severity::as_str().
    severity      TEXT    NOT NULL CHECK (severity IN ('info','good','warn','bad')),
    -- Which project (if any) this alert is about. `NULL` = system-level
    -- (e.g. "SMTP down", "settings changed").
    project_id    UUID    REFERENCES projects (id) ON DELETE SET NULL,
    -- Which individual job (if any). `NULL` = whole-run or system event.
    job_id        TEXT,
    title         TEXT    NOT NULL,
    message       TEXT    NOT NULL,
    -- Free-form JSON payload (counts, durations, proxy URL, schedule
    -- name, etc.). The UI renders specific known shapes per `type`.
    payload       JSONB   NOT NULL DEFAULT '{}'::jsonb,
    is_read       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Notification Center list (newest first, optionally filtered by project
-- and/or unread).
CREATE INDEX IF NOT EXISTS idx_notifications_created
    ON notifications (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notifications_project
    ON notifications (project_id, created_at DESC);
-- Unread count for the bell badge.
CREATE INDEX IF NOT EXISTS idx_notifications_unread
    ON notifications (created_at DESC) WHERE is_read = FALSE;
