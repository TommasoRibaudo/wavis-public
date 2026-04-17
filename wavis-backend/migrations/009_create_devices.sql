-- Create devices table: links machine-specific credentials to a durable user identity.
-- device_id is the primary key (UUID), user_id is a foreign key to users(user_id)
-- with ON DELETE CASCADE. device_name is a human-readable label.
-- revoked_at is nullable (null = active device).
-- device_public_key is nullable (reserved for future use).

CREATE TABLE devices (
    device_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    device_name TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    device_public_key BYTEA
);

CREATE INDEX idx_devices_user_id ON devices (user_id);
