-- The broker sub-ledger: one row per broker statement line, as printed.
-- T11 books these into postings with lots; until then `check` holds them to
-- holding_assertion. Decimals are strings, as in postings.
CREATE TABLE broker_record (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id      INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    external_ref    TEXT NOT NULL UNIQUE,                      -- '<broker>:<account no>:<line key>'
    kind            TEXT NOT NULL
        CHECK (kind IN ('buy', 'sell', 'dividend', 'withholding', 'interest', 'fee',
                        'deposit', 'withdrawal', 'transfer-in', 'transfer-out', 'split',
                        'award-grant', 'award-vesting', 'award-withholding', 'internal',
                        'opening')),
    trade_date      TEXT,                                      -- NULL where the source omits it
    settle_date     TEXT NOT NULL,                             -- the day it moves the balances
    executed_at     TEXT,                                      -- UTC
    symbol          TEXT,
    quantity        TEXT NOT NULL,                             -- signed change in shares of symbol
    price           TEXT NOT NULL,
    amount          TEXT NOT NULL,                             -- amount + commission = change in cash
    commission      TEXT NOT NULL,
    currency        TEXT NOT NULL,
    description     TEXT NOT NULL,
    import_batch_id INTEGER REFERENCES import_batch (id),
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE INDEX idx_broker_record_account ON broker_record (account_id, settle_date);

-- What a broker statement says an account held at the end of `as_of`: cash
-- per currency, shares per security. The sibling of balance_assertion, which
-- is held to postings; this is held to broker_record.
CREATE TABLE holding_assertion (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    as_of      TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('cash', 'position')),
    commodity  TEXT NOT NULL,                                  -- currency code or ticker
    quantity   TEXT NOT NULL,
    source     TEXT NOT NULL CHECK (source IN ('statement')),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (account_id, as_of, kind, commodity, source)
) STRICT;

-- Every outside figure, whichever table holds it: what coverage reports read.
CREATE VIEW assertion_coverage AS
    SELECT account_id, currency AS commodity, period_end AS as_of, source, 'balance' AS kind
    FROM balance_assertion
    UNION ALL
    SELECT account_id, commodity, as_of, source, kind
    FROM holding_assertion;
