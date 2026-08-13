-- 0005: Connect settings + richer comments.
--
-- account_settings: per-account privacy knobs, one row per account,
-- created lazily with defaults matching today's behavior (discoverable,
-- approval-required follows, anyone-who-can-see-the-post may comment).
--
-- comments grow parent_id (one-level replies) and comment_reactions
-- mirror post_reactions.

CREATE TABLE account_settings (
    account_id   TEXT PRIMARY KEY REFERENCES accounts(id),
    discoverable INTEGER NOT NULL DEFAULT 1,  -- appear in search results
    auto_accept  INTEGER NOT NULL DEFAULT 0,  -- follow requests auto-accept
    comments_from TEXT NOT NULL DEFAULT 'viewers'
                  CHECK (comments_from IN ('viewers','followers','off')),
    updated      INTEGER NOT NULL DEFAULT 0
);

ALTER TABLE comments ADD COLUMN parent_id TEXT REFERENCES comments(id);

CREATE TABLE comment_reactions (
    comment_id TEXT NOT NULL REFERENCES comments(id),
    account_id TEXT NOT NULL REFERENCES accounts(id),
    kind       TEXT NOT NULL,
    created    INTEGER NOT NULL,
    PRIMARY KEY (comment_id, account_id)
);
