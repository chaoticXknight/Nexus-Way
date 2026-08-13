-- Adds a non-administrative community role for scoped beta invitation access.

ALTER TABLE accounts ADD COLUMN community_role TEXT NOT NULL DEFAULT 'member'
    CHECK (community_role IN ('member','steward'));
CREATE INDEX idx_accounts_community_role ON accounts(community_role);
