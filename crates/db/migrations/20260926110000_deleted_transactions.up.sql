-- Deleting a transaction a person entered by mistake. The row stays, so its
-- history in transaction_events (ON DELETE RESTRICT) and its dedup ref stay
-- too; its postings go, so it counts toward nothing, and `deleted_at` keeps
-- it out of every list. The 'deleted' event holds what it was.
ALTER TABLE transactions ADD COLUMN deleted_at TEXT;

-- transaction_events gains the 'deleted' kind. A CHECK cannot be altered in
-- place, so the table is rebuilt, rows, index and append-only triggers
-- included.
CREATE TABLE transaction_events_new (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE RESTRICT,
    at             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    kind           TEXT NOT NULL
        CHECK (kind IN ('entered', 'edited', 'split', 'confirmed', 'paired', 'deleted')),
    payload        TEXT NOT NULL CHECK (json_valid(payload))
) STRICT;

INSERT INTO transaction_events_new (id, transaction_id, at, kind, payload)
    SELECT id, transaction_id, at, kind, payload FROM transaction_events;

DROP TABLE transaction_events;

ALTER TABLE transaction_events_new RENAME TO transaction_events;

CREATE INDEX idx_transaction_events_transaction ON transaction_events (transaction_id, id);

CREATE TRIGGER transaction_events_no_update BEFORE UPDATE ON transaction_events
BEGIN
    SELECT RAISE(ABORT, 'transaction_events is append-only');
END;

CREATE TRIGGER transaction_events_no_delete BEFORE DELETE ON transaction_events
BEGIN
    SELECT RAISE(ABORT, 'transaction_events is append-only');
END;
