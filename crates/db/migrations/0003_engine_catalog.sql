-- Feature #3: global engine catalog (imported from the real GSA .ini corpus).
-- App-wide registry of every engine the app knows how to run.  Upsert
-- identity is the namespaced file path (stable across re-imports).
CREATE TABLE IF NOT EXISTS engine_catalog (
    id           BIGSERIAL PRIMARY KEY,
    file_path    TEXT NOT NULL UNIQUE,
    engine_name  TEXT NOT NULL,
    engine_type  TEXT NOT NULL,
    url          TEXT NOT NULL DEFAULT '',
    step_count   INTEGER NOT NULL DEFAULT 0,
    dofollow     BOOLEAN NOT NULL DEFAULT FALSE,
    register     BOOLEAN NOT NULL DEFAULT FALSE,
    category     TEXT NOT NULL DEFAULT 'web' CHECK (category IN ('web','email')),
    imported_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_catalog_type ON engine_catalog (engine_type);
CREATE INDEX IF NOT EXISTS idx_catalog_name ON engine_catalog (engine_name);
