-- Add secret phrase (password in frontend) columns and recovery_id to users table.
-- phrase_salt and phrase_verifier store AES-256-GCM encrypted blobs
-- (nonce || ciphertext || tag), not raw bytes. Application-level encryption
-- happens in domain/phrase.rs before writing to the database.
-- phrase_version tracks the Argon2id parameter version for future upgrades.
-- phrase_enc_version tracks the encryption key version for future key rotation.
-- recovery_id is a human-readable account locator (format: wvs-XXXX-XXXX).

ALTER TABLE users ADD COLUMN phrase_salt BYTEA;
ALTER TABLE users ADD COLUMN phrase_verifier BYTEA;
ALTER TABLE users ADD COLUMN phrase_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE users ADD COLUMN phrase_enc_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE users ADD COLUMN recovery_id TEXT;

-- Partial unique index: only enforce uniqueness on non-null recovery_ids.
-- Allows existing users without a recovery_id to coexist.
CREATE UNIQUE INDEX idx_users_recovery_id ON users (recovery_id) WHERE recovery_id IS NOT NULL;
