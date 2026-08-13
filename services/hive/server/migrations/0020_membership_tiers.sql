-- Distinguishes founding beta membership from future paid membership without granting privileges.

ALTER TABLE accounts ADD COLUMN membership_tier TEXT NOT NULL DEFAULT 'beta'
    CHECK (membership_tier IN ('beta','paid'));
CREATE INDEX idx_accounts_membership_tier ON accounts(membership_tier);
