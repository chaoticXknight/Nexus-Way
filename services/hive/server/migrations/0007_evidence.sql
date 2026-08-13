-- 0007_evidence: report snapshots + legal hold (18 U.S.C. §2258A posture).
--
-- Reported content is snapshotted at filing time so the author deleting it
-- (or a moderation takedown) cannot destroy the evidence trail. Media blobs
-- referenced by a snapshot get legal_hold: GC and refcounting will never
-- delete the bytes while a hold is set. Quarantine hides content from every
-- user but preserves it for law enforcement — the opposite of a delete.

CREATE TABLE report_snapshots (
    report_id   INTEGER PRIMARY KEY REFERENCES reports(id),
    -- Frozen copy of the subject at report time.
    subject_kind TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    author       TEXT NOT NULL DEFAULT '',   -- author account id ('' for account reports)
    author_handle TEXT NOT NULL DEFAULT '',
    body         TEXT NOT NULL DEFAULT '',
    media        TEXT NOT NULL DEFAULT '[]', -- JSON array of blob ids
    audience     TEXT NOT NULL DEFAULT '',
    captured_at  INTEGER NOT NULL
);

-- GC must never remove held bytes (blob.rs run_gc checks this).
ALTER TABLE blobs ADD COLUMN legal_hold INTEGER NOT NULL DEFAULT 0;

-- Quarantined content: hidden from all users (like deleted) but flagged as
-- preserved evidence. deleted_at hides it; quarantined marks WHY.
ALTER TABLE posts ADD COLUMN quarantined INTEGER;
ALTER TABLE comments ADD COLUMN quarantined INTEGER;
