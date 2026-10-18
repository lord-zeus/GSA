-- Feature #15 — Settings & profiles.
--
-- Global settings reuse the existing `settings` key/value table (F#12):
--   key = 'global.settings'  → JSON blob of `ser_engine::settings::Settings`
-- Per-engine overrides get a dedicated sparse table (field = override name,
-- value = display string — the same string vocabulary the GSA dialogs use,
-- so import/export of GSA profile text remains representable):
CREATE TABLE IF NOT EXISTS engine_settings (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    engine_name TEXT NOT NULL,
    field       TEXT NOT NULL,
    value       TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (engine_name, field)
);

-- Seed the global-defaults row (idempotent) so `SELECT ... ` never sees a
-- missing key. An empty `{}` JSON blob = "everything unset / runner default".
INSERT INTO settings (key, value) VALUES ('global.settings', '{}')
ON CONFLICT (key) DO NOTHING;

CREATE INDEX IF NOT EXISTS idx_engine_settings_name
    ON engine_settings (engine_name);

-- Bundle proxy-import uses `ON CONFLICT (host, port)` — the proxies table
-- has no uniqueness on that pair yet, so establish it now (dedup any pre-
-- existing accidental duplicates first):
DELETE FROM proxies p
  USING proxies q
  WHERE p.host = q.host
    AND p.port = q.port
    AND p.id > q.id;
ALTER TABLE proxies
    DROP CONSTRAINT IF EXISTS uq_proxies_host_port;
CREATE UNIQUE INDEX IF NOT EXISTS uq_proxies_host_port
    ON proxies (host, port);
