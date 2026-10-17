-- v2: harden job claim against duplicate pickup under concurrency.
--
-- Postgres does not allow FOR UPDATE on UPDATE's target, and a plain
-- `UPDATE ... WHERE id=(SELECT ... LIMIT 1) ... SET status='running'`
-- can let two concurrent claimers match the same row (both increment
-- `attempts`, both receive a `RETURNING` row).  We fix it the cheap way:
-- add a claim token column and make the UPDATE conditional on the token
-- being empty.  A losing worker then UPDATE-s zero rows and gets None.

ALTER TABLE jobs ADD COLUMN IF NOT EXISTS claimed_by TEXT;
