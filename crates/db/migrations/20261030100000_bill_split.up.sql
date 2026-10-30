-- T08: bill splits, over postings that already carry the balances. A split only
-- groups them, so removing every row here leaves every balance as it was.

-- The explicit people list; a person is never derived from a name on a line.
CREATE TABLE person (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Which split accounts are a person's: "A owes me" sums Split:A, Split:Gear:A
-- and any others listed here. One owner per account.
CREATE TABLE person_account (
    account_id INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE RESTRICT,
    person_id  INTEGER NOT NULL REFERENCES person (id) ON DELETE RESTRICT
) STRICT;

CREATE INDEX idx_person_account_person ON person_account (person_id);

-- `account_id` is the group's account for a netted split, where only the net
-- matters and Splitwise decides who owes whom inside it. `deleted_at` keeps a
-- deleted split's id, which detach events name.
CREATE TABLE split (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('per_person', 'netted')),
    account_id INTEGER REFERENCES accounts (id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    deleted_at TEXT,
    CHECK ((kind = 'netted') = (account_id IS NOT NULL))
) STRICT;

-- One posting's part in one split. `amount` is signed like the posting and may
-- be less than it: one repayment can cover several splits, or pay more than a
-- split still owes. A posting's rows never add up past the posting (app-enforced,
-- since SQL cannot sum decimal strings).
--
-- `person_id` is NULL in a netted split and for 'own', your own share on a line
-- with no split leg. `rewrote` says the engine wrote this line's legs for the
-- split, so detaching returns them to 未分類; a line merely gathered keeps its legs.
--
-- ON DELETE RESTRICT on the posting: the review queue must refuse first.
CREATE TABLE share (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    split_id   INTEGER NOT NULL REFERENCES split (id) ON DELETE RESTRICT,
    posting_id INTEGER NOT NULL REFERENCES postings (id) ON DELETE RESTRICT,
    role       TEXT NOT NULL CHECK (role IN ('share', 'repayment', 'write_off', 'own')),
    person_id  INTEGER REFERENCES person (id) ON DELETE RESTRICT,
    amount     TEXT NOT NULL,                              -- decimal string, like postings.amount
    rewrote    INTEGER NOT NULL DEFAULT 0 CHECK (rewrote IN (0, 1)),
    UNIQUE (split_id, posting_id),
    CHECK (role <> 'own' OR person_id IS NULL)
) STRICT;

CREATE INDEX idx_share_posting ON share (posting_id);

-- transaction_events gains 'attached' and 'detached', whose payload is the split
-- and the rows linked or unlinked. A CHECK cannot be altered in place, so the
-- table is rebuilt, rows, index and append-only triggers included.
CREATE TABLE transaction_events_new (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE RESTRICT,
    at             TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    kind           TEXT NOT NULL
        CHECK (kind IN ('entered', 'edited', 'split', 'confirmed', 'paired', 'deleted',
                        'unconfirmed', 'attached', 'detached')),
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
