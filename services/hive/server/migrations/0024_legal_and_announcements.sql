-- Legal acceptance records and system announcements.
-- legal_acceptances: one row per (account, document, version) — the durable
-- evidence that an account accepted a given version of the terms/privacy
-- policy. Versions are the "Last updated" date parsed from the served text.
-- announcements: operator broadcasts (system notices, maintenance windows,
-- legal-change notices) whose title/body back the "system" notification kind.

CREATE TABLE legal_acceptances (
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    doc         TEXT NOT NULL CHECK (doc IN ('terms','privacy')),
    version     TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    ip          TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, doc, version)
);

CREATE TABLE announcements (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('system','maintenance','legal')),
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,
    created    INTEGER NOT NULL,
    created_by TEXT NOT NULL REFERENCES accounts(id)
);
