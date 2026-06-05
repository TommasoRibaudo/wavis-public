CREATE TABLE bug_report_bans (
    user_id   UUID        PRIMARY KEY,
    banned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    reason    TEXT        NOT NULL DEFAULT ''
);
