-- Adds richer media metadata and durable user-facing feed organization.

ALTER TABLE posts ADD COLUMN media_types TEXT NOT NULL DEFAULT '[]';
ALTER TABLE posts ADD COLUMN alt_text TEXT NOT NULL DEFAULT '';
ALTER TABLE posts ADD COLUMN content_warning TEXT NOT NULL DEFAULT '';
ALTER TABLE posts ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;

CREATE TABLE saved_posts (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    post_id    TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    created    INTEGER NOT NULL,
    PRIMARY KEY (account_id, post_id)
);
CREATE INDEX idx_saved_posts_account_created ON saved_posts(account_id, created DESC);

CREATE TABLE post_revisions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    post_id    TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    body       TEXT NOT NULL,
    replaced_at INTEGER NOT NULL
);
CREATE INDEX idx_post_revisions_post ON post_revisions(post_id, replaced_at DESC);