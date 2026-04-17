-- Create pairings table: stores QR/code device pairing sessions.
-- pairing_id is the primary key (UUID). code_hash stores the HMAC-SHA256
-- hash of the pairing code (plaintext never stored). approved_user_id and
-- approved_by_device_id are set when a trusted device approves the pairing.
-- expires_at enforces a 5-minute TTL. used_at marks completion.
-- attempt_count tracks failed code verification attempts (lockout after 5).

CREATE TABLE pairings (
    pairing_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code_hash BYTEA NOT NULL,
    request_device_name TEXT NOT NULL,
    approved_user_id UUID,
    approved_by_device_id UUID,
    approved_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ,
    attempt_count INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_pairings_expires_at ON pairings (expires_at);
