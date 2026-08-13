-- 0002_blob_uploads: in-progress chunked uploads (Hive_design_doc.md §3.2).
-- Rows here are transient: deleted on commit, or GC'd after 24 h.

CREATE TABLE blob_uploads (
    hash     TEXT PRIMARY KEY,               -- declared content hash = future blob id
    owner    TEXT NOT NULL REFERENCES accounts(id),
    bytes    INTEGER NOT NULL,               -- declared total size
    public   INTEGER NOT NULL DEFAULT 0,
    started  INTEGER NOT NULL
);
CREATE INDEX idx_blob_uploads_started ON blob_uploads(started);
