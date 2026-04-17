-- Migration 011: Migrate refresh_tokens from user_id to device_id binding
-- This migration restructures refresh_tokens to reference devices instead of users,
-- adds family_id for token rotation tracking, and adds consumed_at/revoked_at columns
-- for reuse detection and revocation. The consumed_refresh_tokens table is dropped
-- since consumed state is now tracked inline via consumed_at.

-- Step 1: Add device_id column (nullable initially for backfill)
ALTER TABLE refresh_tokens ADD COLUMN device_id UUID;

-- Step 2: Backfill device_id from user_id
-- This works because migration 010 created devices with device_id = user_id
UPDATE refresh_tokens SET device_id = user_id;

-- Step 3: Make device_id NOT NULL and add FK to devices
ALTER TABLE refresh_tokens ALTER COLUMN device_id SET NOT NULL;
ALTER TABLE refresh_tokens ADD CONSTRAINT fk_refresh_tokens_device
    FOREIGN KEY (device_id) REFERENCES devices(device_id) ON DELETE CASCADE;

-- Step 4: Add family_id (nullable first, backfill, then NOT NULL + default)
ALTER TABLE refresh_tokens ADD COLUMN IF NOT EXISTS family_id UUID;
UPDATE refresh_tokens SET family_id = gen_random_uuid() WHERE family_id IS NULL;
ALTER TABLE refresh_tokens ALTER COLUMN family_id SET NOT NULL;
ALTER TABLE refresh_tokens ALTER COLUMN family_id SET DEFAULT gen_random_uuid();

-- Step 5: Add consumed_at and revoked_at columns
ALTER TABLE refresh_tokens ADD COLUMN IF NOT EXISTS consumed_at TIMESTAMPTZ;
ALTER TABLE refresh_tokens ADD COLUMN IF NOT EXISTS revoked_at TIMESTAMPTZ;

-- Step 6: Drop old user_id index before dropping the column
DROP INDEX IF EXISTS idx_refresh_tokens_user_id;

-- Step 7: Drop user_id column (no longer needed — join through devices for user lookup)
ALTER TABLE refresh_tokens DROP COLUMN user_id;

-- Step 8: Drop consumed_refresh_tokens table (consumed state now tracked via consumed_at)
DROP TABLE IF EXISTS consumed_refresh_tokens;

-- Step 9: Rename id to refresh_id
ALTER TABLE refresh_tokens RENAME COLUMN id TO refresh_id;

-- Step 10: Create indexes for query performance
CREATE INDEX idx_refresh_tokens_device_id ON refresh_tokens (device_id);
CREATE INDEX idx_refresh_tokens_family_id ON refresh_tokens (family_id);
-- Note: idx_refresh_tokens_expires_at already exists from migration 002,
-- but we recreate it to ensure it exists (idempotent)
DROP INDEX IF EXISTS idx_refresh_tokens_expires_at;
CREATE INDEX idx_refresh_tokens_expires_at ON refresh_tokens (expires_at);
