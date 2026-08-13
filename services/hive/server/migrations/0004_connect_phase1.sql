-- 0004: Connect phase 1 — notification persistence (alerts survive app
-- restarts; live connect_notif frames stay the badge path) and device-link
-- requests (§4.2: a new device asks, an enrolled device holding the
-- identity key approves — keys never leave hardware).

CREATE TABLE notifications (
    id         TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id),   -- recipient
    kind       TEXT NOT NULL,   -- follow_request|follow_accepted|post|comment|reaction
    actor      TEXT NOT NULL REFERENCES accounts(id),
    subject_id TEXT NOT NULL DEFAULT '',                -- post id when relevant
    created    INTEGER NOT NULL,
    seen       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_notifications_inbox ON notifications(account_id, created);

CREATE TABLE link_requests (
    code           TEXT PRIMARY KEY,   -- short human code the phone displays
    device_pub     TEXT NOT NULL,
    device_name    TEXT NOT NULL,
    created        INTEGER NOT NULL,
    account_id     TEXT,               -- set on approval
    device_id      TEXT                -- set on approval
);
