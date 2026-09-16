-- A transaction header. Its legs live in postings and must sum to zero per
-- currency; that invariant is enforced in the app (see the postings migration).
CREATE TABLE transactions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL,                             -- ISO date the transaction occurred
    payee           TEXT,
    narration       TEXT,
    source          TEXT NOT NULL
        CHECK (source IN ('import', 'manual', 'tiantian')),   -- tiantian = frozen 天天記帳 history / interim entry

    -- Dedup key: a source line id, or a stable hash of date+amount+desc. Scoped
    -- to `source` so two sources reusing the same line-id scheme cannot collide
    -- (uniqueness is on (source, external_ref); see index below).
    external_ref    TEXT,

    reviewed        INTEGER NOT NULL DEFAULT 0
        CHECK (reviewed IN (0, 1)),                            -- boolean: reviewed/categorised by the user

    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- Dedup guarantee: at most one transaction per (source, external_ref); nulls
-- exempt, for hand-entered rows that have no source line.
CREATE UNIQUE INDEX idx_transactions_external_ref
    ON transactions (source, external_ref)
    WHERE external_ref IS NOT NULL;

CREATE INDEX idx_transactions_date ON transactions (date);
