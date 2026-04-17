CREATE TABLE IF NOT EXISTS channel_invites (
    code TEXT PRIMARY KEY,
    channel_id UUID NOT NULL REFERENCES channels(channel_id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ,
    max_uses INTEGER,
    uses INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (uses >= 0),
    CHECK (max_uses IS NULL OR max_uses > 0),
    CHECK (max_uses IS NULL OR uses <= max_uses)
);

CREATE INDEX idx_channel_invites_channel_id ON channel_invites (channel_id);

CREATE INDEX idx_channel_invites_expires_at ON channel_invites (expires_at);
