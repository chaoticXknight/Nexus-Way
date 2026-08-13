-- Durable grants for end-to-end encrypted WIRE attachment blobs.
CREATE TABLE wire_attachments (
    msg_id         TEXT NOT NULL,
    blob_id        TEXT NOT NULL REFERENCES blobs(id),
    owner_account  TEXT NOT NULL REFERENCES accounts(id),
    created        INTEGER NOT NULL,
    PRIMARY KEY (msg_id, blob_id)
);

CREATE TABLE wire_attachment_grants (
    msg_id      TEXT NOT NULL,
    blob_id     TEXT NOT NULL,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    PRIMARY KEY (msg_id, blob_id, account_id),
    FOREIGN KEY (msg_id, blob_id)
        REFERENCES wire_attachments(msg_id, blob_id) ON DELETE CASCADE
);

CREATE INDEX idx_wire_attachment_grants_account_blob
    ON wire_attachment_grants(account_id, blob_id);
