-- Feature #14 — Geo/IP handling: per-proxy country targeting + egress-IP
-- verification state.
--
-- GSA's own proxy list columns (from the original binary):
--   Host, Port, Type, Speed, Country, Status, Source
-- `country`    → user-set target country (which country should this proxy serve?)
-- `ip`         → verified egress IP of the proxy (what does this proxy actually exit as?)
-- `ip_public`  → whether the verified IP is an RFC 1918 / loopback / link-local
-- `geo_verified_at` → when the IP was last probed + stored
--
-- The **targeting** column (`country`) is the GSA "Country" field: it says
-- "this proxy is for country X".
-- The **verification** columns (`ip`, `ip_public`, `geo_verified_at`) are
-- auto-populated by the `verify_geo` endpoint: they answer "what is this
-- proxy's actual outgoing address, and is it public?"

BEGIN;

ALTER TABLE proxies
  ADD COLUMN IF NOT EXISTS            country         VARCHAR(64)       NULL,
  ADD COLUMN IF NOT EXISTS            ip              VARCHAR(45)       NULL,
  ADD COLUMN IF NOT EXISTS            ip_public       BOOLEAN           NOT NULL DEFAULT FALSE,
  ADD COLUMN IF NOT EXISTS            geo_verified_at TIMESTAMPTZ       NULL;

-- Optional index for filtering proxies by target country (F#14 selector).
CREATE INDEX IF NOT EXISTS idx_proxies_country ON proxies (country) WHERE country IS NOT NULL;

-- Optional index for bulk-verify queries (most recently verified first).
CREATE INDEX IF NOT EXISTS idx_proxies_geo_verified ON proxies (geo_verified_at DESC NULLS LAST);

COMMIT;
