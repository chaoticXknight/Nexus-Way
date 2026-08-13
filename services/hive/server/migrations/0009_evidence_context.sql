-- 0009_evidence_context: freeze social context in report snapshots.
-- The profile can be rewritten and thread comments deleted after a report
-- is filed — so both are captured at filing time, like the IP trail.

-- JSON {display_name, bio, avatar_blob} of the author at filing time.
-- The avatar blob is legal-held alongside post media.
ALTER TABLE report_snapshots ADD COLUMN author_profile TEXT NOT NULL DEFAULT '{}';
-- JSON {post: {...}|null, comments: [...]} — for comment reports, the post
-- it was made on; for both kinds, the surrounding comment thread.
ALTER TABLE report_snapshots ADD COLUMN thread TEXT NOT NULL DEFAULT '{}';
