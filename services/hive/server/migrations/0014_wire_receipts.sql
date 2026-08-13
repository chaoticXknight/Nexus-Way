CREATE TABLE wire_receipts (
    msg_id             TEXT NOT NULL,
    sender_account     TEXT NOT NULL REFERENCES accounts(id),
    recipient_account  TEXT NOT NULL REFERENCES accounts(id),
    sent_at            INTEGER NOT NULL,
    delivered_at       INTEGER,
    received_at        INTEGER,
    read_at            INTEGER,
    expires            INTEGER NOT NULL,
    PRIMARY KEY (msg_id, sender_account, recipient_account)
);

CREATE INDEX idx_wire_receipts_sender
    ON wire_receipts(sender_account, sent_at);