CREATE TABLE IF NOT EXISTS chat_messages (
    message_id     UUID PRIMARY KEY,
    channel_id     UUID,
    room_id        TEXT NOT NULL,
    participant_id TEXT NOT NULL,
    display_name   TEXT NOT NULL,
    text           TEXT NOT NULL CHECK (length(text) <= 2000),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- History queries for channel-based sessions (JoinVoice path)
CREATE INDEX idx_chat_messages_channel_created
    ON chat_messages (channel_id, created_at)
    WHERE channel_id IS NOT NULL;

-- History queries for legacy rooms (Join/CreateRoom path)
CREATE INDEX idx_chat_messages_room_created
    ON chat_messages (room_id, created_at);

-- Purge sweep: find expired rows efficiently
CREATE INDEX idx_chat_messages_created_at
    ON chat_messages (created_at);
