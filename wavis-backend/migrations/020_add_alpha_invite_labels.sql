ALTER TABLE alpha_invites
    ADD COLUMN IF NOT EXISTS label TEXT;

CREATE INDEX IF NOT EXISTS idx_alpha_invites_label ON alpha_invites (label);
