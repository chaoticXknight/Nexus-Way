-- 0006_compliance: right-to-correction edits + COPPA age attestation.
ALTER TABLE posts ADD COLUMN edited INTEGER;
ALTER TABLE comments ADD COLUMN edited INTEGER;
ALTER TABLE accounts ADD COLUMN age_attested INTEGER NOT NULL DEFAULT 0;
