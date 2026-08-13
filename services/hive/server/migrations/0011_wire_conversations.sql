CREATE TABLE wire_conversations (
    account_a    TEXT NOT NULL REFERENCES accounts(id),
    account_b    TEXT NOT NULL REFERENCES accounts(id),
    requested_by TEXT NOT NULL REFERENCES accounts(id),
    state        TEXT NOT NULL CHECK(state IN ('pending', 'accepted')),
    created      INTEGER NOT NULL,
    updated      INTEGER NOT NULL,
    PRIMARY KEY (account_a, account_b),
    CHECK(account_a < account_b),
    CHECK(requested_by = account_a OR requested_by = account_b)
);

CREATE INDEX idx_wire_conversations_participants
    ON wire_conversations(state, account_a, account_b);