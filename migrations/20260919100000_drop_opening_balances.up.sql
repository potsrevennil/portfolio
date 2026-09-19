-- Opening balances are ordinary transactions, the Beancount way: an account
-- leg plus a balancing 'Equity:Opening-Balances' leg dated at the account's
-- start, so every balance is one sum over postings. The loader enforces at most
-- one opening per (account, currency), which this table's primary key used to.
DROP TABLE opening_balances;
