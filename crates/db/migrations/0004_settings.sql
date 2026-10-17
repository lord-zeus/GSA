-- F#12: operator-level email mailbox (GSA "your e-mail" setting).
-- One pair shared by the whole email catalog; `pop3 login=%email%` style
-- specs resolve against it. Empty rows = mailbox not configured.
CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO settings (key, value) VALUES
    ('email.address',    ''),
    ('email.password',   '')
ON CONFLICT (key) DO NOTHING;
