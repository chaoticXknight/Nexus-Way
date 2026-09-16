CREATE TABLE pending_calls (
    call_id TEXT NOT NULL,
    first_account TEXT NOT NULL,
    second_account TEXT NOT NULL,
    recipient TEXT NOT NULL,
    frame TEXT NOT NULL,
    expires INTEGER NOT NULL,
    ended INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (call_id, first_account, second_account)
);
CREATE INDEX pending_calls_recipient ON pending_calls(recipient, ended, expires);