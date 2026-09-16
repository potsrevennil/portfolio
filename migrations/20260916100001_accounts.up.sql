-- Chart of accounts. The primary key is a cheap INTEGER surrogate; the
-- human-readable ASCII path (e.g. 'Assets:Cathay:Savings') is a UNIQUE column
-- and the single source of truth for the hierarchy, so a subtree is queried by
-- prefix (path = 'Assets' OR path LIKE 'Assets:%'). Receivables and payables are
-- ordinary accounts ('Assets:Receivable:<person>', 'Liabilities:Payable:
-- <person>'); the posting sign picks the side, so there is no separate table.
CREATE TABLE accounts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    path        TEXT NOT NULL UNIQUE,                          -- ASCII path, e.g. 'Assets:Cathay:Savings'
    label       TEXT NOT NULL,                                 -- Chinese display label, e.g. '國泰活存'
    type        TEXT NOT NULL
        CHECK (type IN ('asset', 'liability', 'equity', 'income', 'expense')),

    -- Live lifecycle flag: an account can be closed to declutter views and
    -- filters, then reopened. The dated history of every close/reopen lives in
    -- account_events, so reopening never erases the record of a prior closure.
    closed      INTEGER NOT NULL DEFAULT 0
        CHECK (closed IN (0, 1)),

    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE INDEX idx_accounts_type ON accounts (type);
CREATE INDEX idx_accounts_closed ON accounts (closed);

-- Append-only lifecycle audit trail: one row per create/update/close/reopen,
-- never updated or deleted. ON DELETE RESTRICT protects it — an account with
-- history cannot be hard-deleted; retire it with `closed` instead.
CREATE TABLE account_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    event      TEXT NOT NULL
        CHECK (event IN ('created', 'updated', 'closed', 'reopened')),
    at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    note       TEXT
) STRICT;

CREATE INDEX idx_account_events_account ON account_events (account_id, at);

-- Opening balances, one row per (account, currency). A multi-currency account
-- (e.g. brokerage cash in USD + TWD) holds several rows and needs no
-- sub-accounts. Every account should carry at least one; that minimum can't be
-- expressed in DDL, so it is enforced on account creation in the app.
CREATE TABLE opening_balances (
    account_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    currency    TEXT NOT NULL,                                 -- ISO 4217, or a commodity symbol
    amount      TEXT NOT NULL,                                 -- signed decimal string
    date        TEXT NOT NULL,                                 -- ISO-8601 date 'YYYY-MM-DD'
    PRIMARY KEY (account_id, currency)
) STRICT;
