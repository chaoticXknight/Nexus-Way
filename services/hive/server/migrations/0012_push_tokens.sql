CREATE TABLE push_tokens (
    device_id TEXT PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    token TEXT NOT NULL UNIQUE,
    updated INTEGER NOT NULL
);