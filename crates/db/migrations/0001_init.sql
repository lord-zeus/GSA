CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE projects (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE engines (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id   UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    engine_name  TEXT NOT NULL,
    engine_type  TEXT NOT NULL,
    url          TEXT NOT NULL,
    file_path    TEXT NOT NULL,
    step_count   INTEGER NOT NULL DEFAULT 0,
    enabled      BOOLEAN NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, file_path)
);

CREATE INDEX idx_engines_project ON engines (project_id, engine_type);

CREATE TABLE keywords (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    text        TEXT NOT NULL,
    added_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, text)
);

CREATE TABLE jobs (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id   UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    engine_id    UUID REFERENCES engines(id) ON DELETE SET NULL,
    keyword_id   UUID REFERENCES keywords(id) ON DELETE SET NULL,
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending','running','done','failed','rejected','cancelled')),
    attempts     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    started_at   TIMESTAMPTZ,
    finished_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_jobs_project ON jobs (project_id, status);
CREATE INDEX idx_jobs_status ON jobs (status) WHERE status IN ('pending','running');

CREATE TABLE results (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id      UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('success','info','error','rejected')),
    url         TEXT,
    anchor      TEXT,
    message     TEXT,
    payload     JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_results_job ON results (job_id, created_at);

CREATE TABLE history (
    project_id    UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    url           TEXT NOT NULL,
    engine_name   TEXT,
    first_seen    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, url)
);
