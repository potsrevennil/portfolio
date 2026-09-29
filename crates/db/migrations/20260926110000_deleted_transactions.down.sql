-- Back to the kinds before 'deleted'; deletion events have no place there.
CREATE TABLE transaction_events_old (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE RESTRICT,
    at             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    kind           TEXT NOT NULL
        CHECK (kind IN ('entered', 'edited', 'split', 'confirmed', 'paired')),
    payload        TEXT NOT NULL CHECK (json_valid(payload))
) STRICT;

INSERT INTO transaction_events_old (id, transaction_id, at, kind, payload)
    SELECT id, transaction_id, at, kind, payload FROM transaction_events WHERE kind <> 'deleted';

DROP TABLE transaction_events;

ALTER TABLE transaction_events_old RENAME TO transaction_events;

CREATE INDEX idx_transaction_events_transaction ON transaction_events (transaction_id, id);

CREATE TRIGGER transaction_events_no_update BEFORE UPDATE ON transaction_events
BEGIN
    SELECT RAISE(ABORT, 'transaction_events is append-only');
END;

CREATE TRIGGER transaction_events_no_delete BEFORE DELETE ON transaction_events
BEGIN
    SELECT RAISE(ABORT, 'transaction_events is append-only');
END;

ALTER TABLE transactions DROP COLUMN deleted_at;
