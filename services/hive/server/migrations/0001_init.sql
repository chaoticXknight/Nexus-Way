-- 0001_init: core HIVE schema (Hive_design_doc.md §3.1).
-- SQLite now; every type/constraint chosen to port cleanly to Postgres.
-- Timestamps are unix seconds (INTEGER). Keys/hashes are text (hex/base64).

CREATE TABLE accounts (
    id            TEXT PRIMARY KEY,            -- hex(SHA-256(identity_pub))
    identity_pub  TEXT NOT NULL,               -- base64 Ed25519 public key
    handle        TEXT NOT NULL UNIQUE,        -- [a-z0-9_]{3,24}
    kind          TEXT NOT NULL DEFAULT 'personal'
                  CHECK (kind IN ('personal','org')),
    created       INTEGER NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active'
                  CHECK (status IN ('active','suspended','deleted')),
    founder       INTEGER NOT NULL DEFAULT 0,
    tier          TEXT NOT NULL DEFAULT 'free'
                  CHECK (tier IN ('free','paid','family')),
    quota_bytes   INTEGER NOT NULL DEFAULT 0,  -- §3.3: free tier stores no data
    last_seen     INTEGER
);

CREATE TABLE devices (
    id          TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    device_pub  TEXT NOT NULL,                 -- base64 Ed25519
    cert        TEXT NOT NULL,                 -- identity-key-signed device certificate
    name        TEXT NOT NULL DEFAULT '',
    created     INTEGER NOT NULL,
    expiry      INTEGER,
    revoked_at  INTEGER,
    last_ip     TEXT,                          -- device reporting: REAL client IP
    last_seen   INTEGER
);
CREATE INDEX idx_devices_account ON devices(account_id);

CREATE TABLE sessions (
    token_hash  TEXT PRIMARY KEY,              -- hash only: a DB leak leaks no live sessions
    device_id   TEXT NOT NULL REFERENCES devices(id),
    created     INTEGER NOT NULL,
    expires     INTEGER NOT NULL,              -- 30-day sliding TTL
    last_ip     TEXT
);
CREATE INDEX idx_sessions_device ON sessions(device_id);
CREATE INDEX idx_sessions_expires ON sessions(expires);

-- §1.7 recovery escrow: tiny client-encrypted key bundles; HIVE holds only
-- ciphertext. One row per path (password / questions / phrase).
CREATE TABLE recovery_escrow (
    account_id     TEXT NOT NULL REFERENCES accounts(id),
    path           TEXT NOT NULL CHECK (path IN ('password','questions','phrase')),
    blob           BLOB NOT NULL,              -- AES-256-GCM ciphertext of the key bundle
    questions      TEXT,                       -- JSON array of the user-written questions (path='questions')
    attempts_today INTEGER NOT NULL DEFAULT 0, -- rate limit: 5/day/path
    attempts_reset INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, path)
);

CREATE TABLE org_members (
    org_id      TEXT NOT NULL REFERENCES accounts(id),
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    role        TEXT NOT NULL CHECK (role IN ('owner','admin','member','poster')),
    added_by    TEXT NOT NULL,
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (org_id, account_id)
);

-- Append-only: auth events, org changes, moderation, recovery attempts.
CREATE TABLE audit (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    actor   TEXT NOT NULL,
    action  TEXT NOT NULL,
    subject TEXT NOT NULL DEFAULT '',
    at      INTEGER NOT NULL,
    ip      TEXT
);
CREATE INDEX idx_audit_actor ON audit(actor, at);

CREATE TABLE invites (
    code     TEXT PRIMARY KEY,                 -- 12-byte hex
    issuer   TEXT NOT NULL REFERENCES accounts(id),
    created  INTEGER NOT NULL,
    used_by  TEXT,
    used_at  INTEGER
);
CREATE INDEX idx_invites_issuer ON invites(issuer);

-- §3.2 blob store: content-addressed, client-side encrypted (public Connect
-- media is the only plaintext class), refcounted with nightly GC.
CREATE TABLE blobs (
    id           TEXT PRIMARY KEY,             -- hex SHA-256 of stored bytes
    owner        TEXT NOT NULL REFERENCES accounts(id),
    bytes        INTEGER NOT NULL,
    created      INTEGER NOT NULL,
    refcount     INTEGER NOT NULL DEFAULT 0,
    public       INTEGER NOT NULL DEFAULT 0,
    storage_path TEXT NOT NULL                 -- blobs/ab/cd/<hash>
);
CREATE INDEX idx_blobs_owner ON blobs(owner);
CREATE INDEX idx_blobs_gc ON blobs(refcount, created);

-- §4 vault sync: opaque encrypted snapshots with sequence discipline.
CREATE TABLE vault_snapshots (
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    seq         INTEGER NOT NULL,
    device_id   TEXT NOT NULL,
    blob_id     TEXT NOT NULL REFERENCES blobs(id),
    created     INTEGER NOT NULL,
    PRIMARY KEY (account_id, seq)
);

-- §5 WIRE relay: ciphertext mailboxes, one row per recipient device.
CREATE TABLE wire_mailbox (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    recipient    TEXT NOT NULL,                -- device id
    sender_hint  TEXT NOT NULL DEFAULT '',     -- sealed; true sender is inside the E2E layer
    msg_id       TEXT NOT NULL,
    envelope     BLOB NOT NULL,
    created      INTEGER NOT NULL,
    delivered_at INTEGER,
    expires      INTEGER NOT NULL              -- 30-day TTL unacked
);
CREATE INDEX idx_mailbox_recipient ON wire_mailbox(recipient, created);
CREATE INDEX idx_mailbox_expires ON wire_mailbox(expires);

CREATE TABLE wire_prekeys (
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    key_id      TEXT NOT NULL,
    prekey_pub  TEXT NOT NULL,                 -- base64 X25519
    signed      TEXT NOT NULL,                 -- identity-key signature
    used        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, key_id)
);

-- §6 Connect.
CREATE TABLE profiles (
    account_id   TEXT PRIMARY KEY REFERENCES accounts(id),
    display_name TEXT NOT NULL DEFAULT '',
    avatar_blob  TEXT,
    bio          TEXT NOT NULL DEFAULT '',
    badge        TEXT NOT NULL DEFAULT ''
);

CREATE TABLE follows (
    follower  TEXT NOT NULL REFERENCES accounts(id),
    followee  TEXT NOT NULL REFERENCES accounts(id),
    state     TEXT NOT NULL CHECK (state IN ('requested','accepted')),
    created   INTEGER NOT NULL,
    PRIMARY KEY (follower, followee)
);
CREATE INDEX idx_follows_followee ON follows(followee, state);

CREATE TABLE blocks (
    blocker  TEXT NOT NULL REFERENCES accounts(id),
    blocked  TEXT NOT NULL REFERENCES accounts(id),
    created  INTEGER NOT NULL,
    PRIMARY KEY (blocker, blocked)
);

CREATE TABLE posts (
    id         TEXT PRIMARY KEY,
    author     TEXT NOT NULL REFERENCES accounts(id),
    created    INTEGER NOT NULL,
    kind       TEXT NOT NULL DEFAULT 'text' CHECK (kind IN ('text','photo')),
    body       TEXT NOT NULL DEFAULT '',      -- plaintext or ciphertext per audience
    media      TEXT NOT NULL DEFAULT '[]',    -- JSON array of blob ids
    audience   TEXT NOT NULL DEFAULT 'public',-- public | followers | circle:<id>
    deleted_at INTEGER
);
CREATE INDEX idx_posts_feed ON posts(author, created);

CREATE TABLE post_reactions (
    post_id    TEXT NOT NULL REFERENCES posts(id),
    account_id TEXT NOT NULL REFERENCES accounts(id),
    kind       TEXT NOT NULL,
    PRIMARY KEY (post_id, account_id)
);

CREATE TABLE comments (
    id         TEXT PRIMARY KEY,
    post_id    TEXT NOT NULL REFERENCES posts(id),
    author     TEXT NOT NULL REFERENCES accounts(id),
    body       TEXT NOT NULL,
    created    INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX idx_comments_post ON comments(post_id, created);

-- "groups" is a SQLite keyword; named social_groups to keep queries unquoted.
CREATE TABLE social_groups (
    id       TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    owner    TEXT NOT NULL REFERENCES accounts(id),
    privacy  TEXT NOT NULL DEFAULT 'private' CHECK (privacy IN ('private','public')),
    created  INTEGER NOT NULL
);

CREATE TABLE group_members (
    group_id   TEXT NOT NULL REFERENCES social_groups(id),
    account_id TEXT NOT NULL REFERENCES accounts(id),
    role       TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('owner','admin','member')),
    PRIMARY KEY (group_id, account_id)
);

-- §6.4 circles: HIVE holds only per-member wrapped (sealed) circle keys.
CREATE TABLE circles (
    id           TEXT PRIMARY KEY,
    owner        TEXT NOT NULL REFERENCES accounts(id),
    name         TEXT NOT NULL,
    wrapped_keys TEXT NOT NULL DEFAULT '{}'   -- JSON {member_account_id: sealed_key_b64}
);

CREATE TABLE reports (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    reporter     TEXT NOT NULL REFERENCES accounts(id),
    subject_kind TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    reason       TEXT NOT NULL DEFAULT '',
    created      INTEGER NOT NULL,
    resolved_at  INTEGER,
    resolution   TEXT
);
