CREATE TABLE IF NOT EXISTS consumed_refresh_tokens (
    token_hash BYTEA PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    consumed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_consumed_refresh_tokens_consumed_at ON consumed_refresh_tokens (consumed_at);
CREATE INDEX idx_consumed_refresh_tokens_user_id ON consumed_refresh_tokens (user_id);
