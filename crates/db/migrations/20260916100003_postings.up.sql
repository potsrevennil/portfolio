-- The legs of a transaction, and what every balance is folded from.
--
-- Amounts are TEXT decimal strings (not integer minor-units): they round-trip
-- rust_decimal exactly for any currency, need no per-currency scale, and let
-- `currency` double as a commodity symbol where "minor unit" has no meaning.
-- The cost: SQL can't sum them without CAST to REAL, which reintroduces float
-- error. So both derived balances and the zero-sum-per-currency invariant are
-- computed in Rust with rust_decimal, never with SQL SUM. A trigger couldn't
-- enforce the invariant anyway, since postings are inserted one row at a time.
--
-- Securities cost/lot fields are intentionally deferred to a later migration.
CREATE TABLE postings (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,

    amount         TEXT NOT NULL,                              -- signed decimal string (quantity for commodities)
    currency       TEXT NOT NULL,                              -- ISO 4217 code, or a commodity/stock symbol

    tags           TEXT                                        -- optional; free-form, parsed by the app
) STRICT;

CREATE INDEX idx_postings_transaction ON postings (transaction_id);
-- Covers both "all currencies for an account" and single-currency balance
-- queries; the account_id prefix serves account-only lookups too.
CREATE INDEX idx_postings_account ON postings (account_id, currency);
