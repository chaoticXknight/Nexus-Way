-- Separates bounded Connect media from general private-storage quota.

ALTER TABLE blobs ADD COLUMN purpose TEXT NOT NULL DEFAULT 'general'
    CHECK (purpose IN ('general','connect_media'));
ALTER TABLE blob_uploads ADD COLUMN purpose TEXT NOT NULL DEFAULT 'general'
    CHECK (purpose IN ('general','connect_media'));

CREATE INDEX idx_blobs_owner_purpose ON blobs(owner, purpose);
CREATE INDEX idx_blob_uploads_owner_purpose ON blob_uploads(owner, purpose);
