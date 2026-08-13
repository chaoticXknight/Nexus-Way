-- Adds temporary posting restrictions without disabling account read access.

ALTER TABLE accounts ADD COLUMN muted_until INTEGER;
CREATE INDEX idx_accounts_muted_until ON accounts(muted_until);
