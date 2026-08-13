CREATE TABLE wire_device_keys (
    device_id  TEXT PRIMARY KEY REFERENCES devices(id),
    wire_pub   TEXT NOT NULL,
    signature  TEXT NOT NULL,
    updated    INTEGER NOT NULL
);

CREATE UNIQUE INDEX idx_wire_mailbox_msg_recipient
    ON wire_mailbox(msg_id, recipient);