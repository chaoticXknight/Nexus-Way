-- Adds explicit Fold membership/key epochs and two-way user-to-Console support threads.

ALTER TABLE circles ADD COLUMN key_epoch INTEGER NOT NULL DEFAULT 1;
ALTER TABLE circles ADD COLUMN key_stale INTEGER NOT NULL DEFAULT 0;
ALTER TABLE posts ADD COLUMN fold_epoch INTEGER;
ALTER TABLE comments ADD COLUMN fold_epoch INTEGER;

CREATE TABLE fold_members (
    circle_id   TEXT NOT NULL REFERENCES circles(id) ON DELETE CASCADE,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    role        TEXT NOT NULL CHECK (role IN ('owner','member')),
    state       TEXT NOT NULL CHECK (state IN ('invited','active')),
    invited_by  TEXT NOT NULL REFERENCES accounts(id),
    invited_at  INTEGER NOT NULL,
    joined_at   INTEGER,
    wrapped_key TEXT,
    key_epoch   INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (circle_id, account_id)
);
CREATE INDEX idx_fold_members_account ON fold_members(account_id, state, circle_id);
CREATE INDEX idx_fold_members_circle ON fold_members(circle_id, state, account_id);

INSERT INTO fold_members
    (circle_id, account_id, role, state, invited_by, invited_at, joined_at, wrapped_key, key_epoch)
SELECT c.id, c.owner, 'owner', 'active', c.owner, c.created, c.created,
       (SELECT value FROM json_each(c.wrapped_keys) WHERE key=c.owner), 1
FROM circles c;

INSERT OR IGNORE INTO fold_members
    (circle_id, account_id, role, state, invited_by, invited_at, joined_at, wrapped_key, key_epoch)
SELECT c.id, j.key, 'member', 'active', c.owner, c.created, c.created,
    j.value, 1
FROM circles c, json_each(c.wrapped_keys) j
WHERE j.key <> c.owner;

CREATE TABLE support_threads (
    id          TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    category    TEXT NOT NULL CHECK (category IN ('bug','feature','help','other')),
    subject     TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','closed')),
    created     INTEGER NOT NULL,
    updated     INTEGER NOT NULL
);
CREATE INDEX idx_support_threads_account ON support_threads(account_id, updated DESC);
CREATE INDEX idx_support_threads_status ON support_threads(status, updated DESC);

CREATE TABLE support_messages (
    id          TEXT PRIMARY KEY,
    thread_id   TEXT NOT NULL REFERENCES support_threads(id) ON DELETE CASCADE,
    sender      TEXT NOT NULL,
    sender_role TEXT NOT NULL CHECK (sender_role IN ('user','admin')),
    body        TEXT NOT NULL,
    created     INTEGER NOT NULL,
    read_at     INTEGER
);
CREATE INDEX idx_support_messages_thread ON support_messages(thread_id, created);
CREATE INDEX idx_support_messages_unread ON support_messages(sender_role, read_at, created);
