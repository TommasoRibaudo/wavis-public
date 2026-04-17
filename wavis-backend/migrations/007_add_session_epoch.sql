-- Add session_epoch column to users table.
-- Incremented on security events (e.g. refresh token reuse detection)
-- to immediately invalidate all outstanding access tokens for a user.
-- Access tokens embed the epoch at signing time; validation rejects
-- tokens whose epoch does not match the current DB value.
ALTER TABLE users ADD COLUMN session_epoch INTEGER NOT NULL DEFAULT 0;
