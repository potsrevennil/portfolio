-- T05: what stopped an import, kept after its rollback so the review queue can
-- show it and a person can settle it before the import is run again.
--
-- A statement line that fits several unverified records stays in
-- verification_choice; every other stop is a row here. `problem` is the typed
-- stop as JSON, `pick` what a person chose where the stop asks for a choice
-- (a 天天記帳 record's line or account), read by the next import. A stop
-- settled by a write (splitting a record, moving its leg) is `resolved_at`;
-- the write itself is in transaction_events.
--
-- Each run of an importer clears what it no longer meets: a refused run
-- replaces its open stops with the ones it found, a successful one clears
-- them all. A pick survives a refusal for another reason, and is dropped only
-- when its own stop is met again: the pick no longer fits.
CREATE TABLE import_stop (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source      TEXT NOT NULL,                     -- the importer, as import_batch.source names it
    kind        TEXT NOT NULL,
    subject     TEXT NOT NULL,                     -- what it is about: a record's ref, a transaction id
    problem     TEXT NOT NULL CHECK (json_valid(problem)),
    pick        TEXT CHECK (pick IS NULL OR json_valid(pick)),
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    resolved_at TEXT,
    cleared_at  TEXT,
    UNIQUE (kind, subject)
) STRICT;

-- Splitting a record in two so each bank's leg can keep its own date: the
-- 'detached' kind, on the new half, {"from", "before": null, "after"}. A line
-- picked for a 天天記帳 record is 'paired' with {"record_ref", "date",
-- "amount"}. The CHECK cannot be altered in place, so transaction_events is
-- rebuilt again.
CREATE TABLE transaction_events_new (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE RESTRICT,
    at             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    kind           TEXT NOT NULL
        CHECK (kind IN ('entered', 'edited', 'split', 'confirmed', 'paired', 'deleted', 'unconfirmed',
                        'detached')),
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
