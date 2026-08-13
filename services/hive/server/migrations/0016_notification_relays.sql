-- Wake-only credentials for the separate Nexus Notify Android package.
CREATE TABLE notification_relays (
    device_id  TEXT PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    updated    INTEGER NOT NULL
);