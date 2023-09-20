-- Add migration script here
CREATE TABLE IF NOT EXISTS commands (
    user_id TEXT NOT NULL,
    command TEXT NOT NULL,
    created_at TEXT NOT NULL,
    parameters TEXT
);