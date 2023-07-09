-- Add migration script here
CREATE TABLE channel_limits (
    channel_id INTEGER NOT NULL,
    channel_limit INTEGER NOT NULL,
    PRIMARY KEY (channel_id)
)