-- What an outside party says an account held: the founding invariant's
-- reference figures. `check` compares each row with the postings.
--
-- Dates are inclusive: `opening` is the balance before `period_start`, and
-- `closing` is the balance at the end of `period_end`. Each figure covers the
-- account and its subtree, in one currency. A point assertion (a 天天記帳 closing,
-- or a counted cash balance) has no period_start and no opening.
CREATE TABLE balance_assertion (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id   INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    currency     TEXT NOT NULL,
    source       TEXT NOT NULL
        CHECK (source IN ('statement', 'tiantian', 'counted')),
    period_start TEXT,
    opening      TEXT,                                         -- decimal string, like postings.amount
    period_end   TEXT NOT NULL,
    closing      TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK ((period_start IS NULL) = (opening IS NULL)),
    CHECK (period_start IS NULL OR period_start <= period_end),
    UNIQUE (account_id, currency, source, period_end)
) STRICT;
