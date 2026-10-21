-- F#33 / T5 — GSA "site list": the discovered target sites for a project.
-- Search (the site finder over the se.dat catalog) produces these; the
-- runner then uses them as the base page for discovery engines (engines
-- with no own base URL — guestbooks / URL engines), exactly like GSA:
-- search finds the site, the engine posts the backlink there.
--
-- `url` is unique per project (upsert on re-discovery); `origin` records
-- where the URL came in from ("search" = site finder, "manual" = user).
CREATE TABLE IF NOT EXISTS project_sites (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id     UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    url            TEXT NOT NULL,
    origin         TEXT NOT NULL DEFAULT 'manual',
    first_found_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, url)
);
CREATE INDEX IF NOT EXISTS idx_project_sites_project ON project_sites(project_id);
