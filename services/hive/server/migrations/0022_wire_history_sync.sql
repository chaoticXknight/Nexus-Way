-- Adds per-device E2E WIRE history snapshot offers and sync cursors.

CREATE TABLE wire_history_sync (
    snapshot_id    TEXT PRIMARY KEY,
    account_id     TEXT NOT NULL REFERENCES accounts(id),
    publisher      TEXT NOT NULL REFERENCES devices(id),
    target_device  TEXT NOT NULL REFERENCES devices(id),
    snapshot_hash  TEXT NOT NULL,
    snapshot       BLOB NOT NULL,
    envelope       BLOB NOT NULL,
    created        INTEGER NOT NULL,
    expires        INTEGER NOT NULL,
    consumed_at    INTEGER,
    UNIQUE (target_device, snapshot_id)
);
CREATE INDEX idx_wire_history_sync_target
    ON wire_history_sync(target_device, consumed_at, created);
CREATE INDEX idx_wire_history_sync_expires
    ON wire_history_sync(expires);

CREATE TABLE wire_history_sync_state (
    account_id     TEXT NOT NULL REFERENCES accounts(id),
    device_id      TEXT NOT NULL REFERENCES devices(id),
    synced_through INTEGER NOT NULL DEFAULT 0,
    updated        INTEGER NOT NULL,
    PRIMARY KEY (account_id, device_id)
);
