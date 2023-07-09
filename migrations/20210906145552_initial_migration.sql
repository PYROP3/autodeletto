-- Add migration script here
CREATE TABLE IF NOT EXISTS channel_limits (
    channel_id TEXT NOT NULL,
    channel_limit INTEGER NOT NULL,
    PRIMARY KEY (channel_id)
);

CREATE TABLE IF NOT EXISTS channel_limit_edits (
    user_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    channel_limit INTEGER NOT NULL,
    created_at TEXT NOT NULL
);