-- Add account-level usernames and backfill from the most recent active device label.
ALTER TABLE users ADD COLUMN IF NOT EXISTS username TEXT NOT NULL DEFAULT '';

UPDATE users u
SET username = COALESCE((
    SELECT device_name
    FROM devices
    WHERE devices.user_id = u.user_id AND revoked_at IS NULL
    ORDER BY created_at DESC
    LIMIT 1
), '')
WHERE u.username = '';
