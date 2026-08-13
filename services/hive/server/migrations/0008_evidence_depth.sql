-- 0008_evidence_depth: deepen report snapshots (CONSOLE-SPEC §5.2).
-- Connection metadata (device IPs, sessions) is purged at 90 days for data
-- minimization — so the author's network trail must be FROZEN at filing
-- time or it won't exist when law enforcement asks for it.

ALTER TABLE report_snapshots ADD COLUMN content_created INTEGER NOT NULL DEFAULT 0;
ALTER TABLE report_snapshots ADD COLUMN content_edited INTEGER;
-- For comment reports: the post the comment was made on.
ALTER TABLE report_snapshots ADD COLUMN parent_post_id TEXT NOT NULL DEFAULT '';
-- JSON array of the author's device/session IP records at filing time:
-- [{"device","name","ip","last_seen"}...] — survives the 90-day purge.
ALTER TABLE report_snapshots ADD COLUMN author_ips TEXT NOT NULL DEFAULT '[]';
-- The reporter's IP at filing (their audit row is purge-exempt, but keep
-- the snapshot self-contained).
ALTER TABLE report_snapshots ADD COLUMN reporter_ip TEXT NOT NULL DEFAULT '';

-- ToS forfeiture: once enforcement action is taken against an account
-- (suspend / takedown / quarantine), it forfeits data-minimization
-- protections. evidence_hold freezes the account's ENTIRE footprint:
-- audit rows, device IPs, and sessions are exempt from the 90-day purge,
-- and every blob the account owns is exempt from GC.
ALTER TABLE accounts ADD COLUMN evidence_hold INTEGER;  -- unix ts when triggered
