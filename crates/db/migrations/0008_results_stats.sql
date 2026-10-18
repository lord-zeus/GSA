-- F#16: Results & outcomes analytics (GSA "stats" dashboard).
--
-- One new column on the existing `results` table:
--   * captchas — true when this outcome was blocked by a CAPTCHA wall.
--     GSA counts captcha walls as a distinct metric (not a plain failure):
--     it's the signal that "this engine/site needs a proxy or a better
--     fingerprint", and operators track that number separately.

ALTER TABLE results
    ADD COLUMN IF NOT EXISTS captchas BOOLEAN NOT NULL DEFAULT FALSE;

-- Rollup queries (per-day, per-engine, per-project) all filter on these:
CREATE INDEX IF NOT EXISTS idx_results_created      ON results (created_at);
CREATE INDEX IF NOT EXISTS idx_results_job          ON results (job_id, created_at);
