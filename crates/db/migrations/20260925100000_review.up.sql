-- T9: what the review queue writes.

-- How a leg's account was chosen, where a machine chose it: 'tiantian' (from
-- the 天天記帳 record the line was matched to), 'rule' (a mapping.toml
-- description rule), 'fallback' (no rule: the uncategorised account), or
-- 'manual' (a person set it). NULL where nothing recorded it, as in history
-- loaded from the journal.
ALTER TABLE postings ADD COLUMN origin TEXT
    CHECK (origin IN ('tiantian', 'rule', 'fallback', 'manual'));

-- Append-only history of every change the app makes to a transaction. An
-- edit overwrites the header and postings in place, so without this row the
-- previous state would be gone.
--
-- `payload` is JSON. For 'entered', 'edited', 'split' and 'confirmed' it is
-- {"before": <snapshot> | null, "after": <snapshot>}, a snapshot being the
-- whole transaction: {"date", "payee", "narration", "reviewed", "legs":
-- [{"account", "amount", "currency", "tags", "origin"}]}, accounts by path and
-- amounts as decimal strings. Whole snapshots rather than a diff: any past
-- state reads off one row, without replaying the ones before it, and a later
-- column needs no new event shape. For 'paired' it is {"statement_ref",
-- "date", "amount"}: the statement line a person chose this record for.
--
-- ON DELETE RESTRICT: a transaction with history cannot be deleted, which is
-- the point; an edit is an update.
CREATE TABLE transaction_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE RESTRICT,
    at             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    kind           TEXT NOT NULL
        CHECK (kind IN ('entered', 'edited', 'split', 'confirmed', 'paired')),
    payload        TEXT NOT NULL CHECK (json_valid(payload))
) STRICT;

CREATE INDEX idx_transaction_events_transaction ON transaction_events (transaction_id, id);

CREATE TRIGGER transaction_events_no_update BEFORE UPDATE ON transaction_events
BEGIN
    SELECT RAISE(ABORT, 'transaction_events is append-only');
END;

CREATE TRIGGER transaction_events_no_delete BEFORE DELETE ON transaction_events
BEGIN
    SELECT RAISE(ABORT, 'transaction_events is append-only');
END;

-- A statement line the importer could not pair on its own: it fits more than
-- one unverified record (two lunches of one price), or its one record fits
-- another line too. The import stops and records the line here; a person picks
-- the record, and the next import verifies it with that one.
--
-- `candidates` is the JSON array of transaction ids the importer saw fit,
-- kept as it saw them so the choice offered is exactly the one it refused to
-- make. `transaction_id` is the pick, NULL until there is one. The row stays
-- after the import uses it: the line's ref then sits in transaction_refs.
CREATE TABLE verification_choice (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    currency       TEXT NOT NULL,
    statement_ref  TEXT NOT NULL UNIQUE,
    date           TEXT NOT NULL,
    amount         TEXT NOT NULL,                              -- decimal string, like postings.amount
    description    TEXT NOT NULL,
    candidates     TEXT NOT NULL CHECK (json_valid(candidates)),
    transaction_id INTEGER REFERENCES transactions (id) ON DELETE RESTRICT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    chosen_at      TEXT,
    CHECK ((transaction_id IS NULL) = (chosen_at IS NULL))
) STRICT;
