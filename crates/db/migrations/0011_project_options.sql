-- F#19 — Anchor intelligence (GSA "Anchor Text" tab options)
--   anchor_mix: GSA-style per-type anchor pool + % weights, e.g.
--     {"secondary":["best coffee shop","best tea shop"],
--      "branding":["example.com|example.com blog"],
--      "generic":["click here","read more"],
--      "domain":true,
--      "percentages":{"secondary":10,"branding":70,"generic":5,"domain":15}}
--     Weight of the *main keyword* anchor = 100 - sum(percentages).
--   use_random_url: GSA "Use a random URL from above on every submission
--     (else in sequence)" — pick from the project URL pool per job.
--   url_variations: GSA "Use URL variations with 20%" (scheme/www/case).
--   root_url: GSA "Use the root/main URL with 30%" (strip the path).
--   linkwheel: GSA "Use verified URLs of another project (Multi-Tier)" —
--     submit verified URLs of these project ids (one per job, round-robin)
--     as additional backlink targets for THIS project's keyword.
-- F#24-lite — auto-export verified URLs to <data_dir>/verified/<project>.csv
--   (GSA "Automatically export verified URLs" + the External-APIs feed).

ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS anchor_mix TEXT,
    ADD COLUMN IF NOT EXISTS use_random_url BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS url_variations INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS root_url INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS linkwheel UUID[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS auto_export BOOLEAN NOT NULL DEFAULT TRUE;
