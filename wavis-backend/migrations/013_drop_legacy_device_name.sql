-- Migration 013: Drop legacy device_name column from users table.
-- device_name now lives on the devices table (created in migration 009).
-- Uses IF EXISTS for safety — the column may not be present in all environments.
ALTER TABLE users DROP COLUMN IF EXISTS device_name;
