-- 0003_connect: the Connect tables themselves shipped in 0001 (§3.1 schema).
-- This migration adds the columns the service layer needs beyond that
-- baseline: freshness timestamps for profiles/reactions/circles.

ALTER TABLE profiles ADD COLUMN updated INTEGER NOT NULL DEFAULT 0;
ALTER TABLE post_reactions ADD COLUMN created INTEGER NOT NULL DEFAULT 0;
ALTER TABLE circles ADD COLUMN created INTEGER NOT NULL DEFAULT 0;
