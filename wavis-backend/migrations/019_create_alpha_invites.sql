CREATE TABLE IF NOT EXISTS alpha_invites (
    invite_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    code_hash BYTEA NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    disabled_at TIMESTAMPTZ,
    max_redemptions INTEGER NOT NULL DEFAULT 1,
    redemption_count INTEGER NOT NULL DEFAULT 0,
    CHECK (max_redemptions > 0),
    CHECK (redemption_count >= 0),
    CHECK (redemption_count <= max_redemptions)
);

CREATE INDEX IF NOT EXISTS idx_alpha_invites_expires_at ON alpha_invites (expires_at);
CREATE INDEX IF NOT EXISTS idx_alpha_invites_disabled_at ON alpha_invites (disabled_at);

CREATE TABLE IF NOT EXISTS alpha_invite_redemptions (
    redemption_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    invite_id UUID NOT NULL REFERENCES alpha_invites(invite_id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    device_id UUID NOT NULL REFERENCES devices(device_id) ON DELETE CASCADE,
    redeemed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_alpha_invite_redemptions_invite_id
    ON alpha_invite_redemptions (invite_id);
CREATE INDEX IF NOT EXISTS idx_alpha_invite_redemptions_user_id
    ON alpha_invite_redemptions (user_id);
