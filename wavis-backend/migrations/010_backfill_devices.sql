-- TRANSITIONAL BACKFILL ONLY: device_id = user_id is a one-time migration artifact
-- so that existing refresh tokens (which reference user_id) can be re-pointed to
-- a device_id without data loss. This equality will NOT hold for any device created
-- after this migration. No application code should assume device_id == user_id.
-- All new devices (registration, pairing, recovery) MUST use gen_random_uuid().
--
-- The users table has no device_name column, so we default to empty string.
INSERT INTO devices (device_id, user_id, device_name, created_at)
SELECT user_id, user_id, '', created_at
FROM users
ON CONFLICT (device_id) DO NOTHING;
