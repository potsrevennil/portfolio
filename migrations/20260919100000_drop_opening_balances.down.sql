CREATE TABLE opening_balances (
    account_id  INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    currency    TEXT NOT NULL,                                 -- ISO 4217, or a commodity symbol
    amount      TEXT NOT NULL,                                 -- signed decimal string
    date        TEXT NOT NULL,                                 -- ISO-8601 date 'YYYY-MM-DD'
    PRIMARY KEY (account_id, currency)
) STRICT;
