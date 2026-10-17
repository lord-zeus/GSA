-- F#13: proxy support (GSA "proxy list/type/test" vocabulary).
-- One table of proxy servers; `use_proxies` per project gates whether the
-- runner routes that project's jobs through the list (GSA "random proxy"
-- behavior: round-robin over the enabled proxies, down ones skipped).
CREATE TABLE IF NOT EXISTS proxies (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    host         TEXT NOT NULL,
    port         INTEGER NOT NULL,
    proxy_kind   TEXT NOT NULL DEFAULT 'http'
                 CHECK (proxy_kind IN ('http', 'socks5')),
    login        TEXT NOT NULL DEFAULT '',
    password     TEXT NOT NULL DEFAULT '',
    enabled      BOOLEAN NOT NULL DEFAULT TRUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_proxies_enabled ON proxies (enabled);

-- Gate flag on projects: OFF = direct connection (previous behavior).
ALTER TABLE projects ADD COLUMN IF NOT EXISTS use_proxies BOOLEAN NOT NULL DEFAULT FALSE;
