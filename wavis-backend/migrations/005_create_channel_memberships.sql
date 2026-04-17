CREATE TABLE IF NOT EXISTS channel_memberships (
    channel_id UUID NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(user_id),
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    banned_at TIMESTAMPTZ,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel_id, user_id)
);

CREATE INDEX idx_channel_memberships_user_id ON channel_memberships (user_id);

CREATE UNIQUE INDEX idx_channel_memberships_one_owner
    ON channel_memberships (channel_id) WHERE role = 'owner';
