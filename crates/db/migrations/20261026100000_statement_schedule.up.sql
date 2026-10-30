-- How often an account's own figure (a statement, or a count) should arrive,
-- and the bank the 對帳進度 page files it under; the reader sets both on that
-- page. No row: never due, filed under its parent account's label.
CREATE TABLE statement_schedule (
    account_id INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    bank       TEXT,                                           -- NULL: the parent account's label
    cadence    TEXT NOT NULL DEFAULT 'never'
        CHECK (cadence IN ('never', 'monthly', 'quarterly', 'yearly', 'count', 'instalment', 'payment')),
    month      INTEGER CHECK (month BETWEEN 1 AND 12),
    day        INTEGER CHECK (day BETWEEN 1 AND 31),           -- 31: the month's end
    every      INTEGER CHECK (every >= 1),                     -- days between counts
    grace      INTEGER NOT NULL CHECK (grace >= 0),            -- days a figure may run late
    since      TEXT,                                           -- ISO date; records before it cannot be checked
    CHECK ((cadence IN ('monthly', 'yearly', 'payment')) = (day IS NOT NULL)),
    CHECK ((cadence = 'yearly') = (month IS NOT NULL)),
    CHECK ((cadence = 'count') = (every IS NOT NULL))
) STRICT;
