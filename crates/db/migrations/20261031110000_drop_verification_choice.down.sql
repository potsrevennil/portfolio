CREATE TABLE verification_choice (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE RESTRICT,
    currency       TEXT NOT NULL,
    statement_ref  TEXT NOT NULL UNIQUE,
    date           TEXT NOT NULL,
    amount         TEXT NOT NULL,
    description    TEXT NOT NULL,
    candidates     TEXT NOT NULL CHECK (json_valid(candidates)),
    transaction_id INTEGER REFERENCES transactions (id) ON DELETE RESTRICT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    chosen_at      TEXT,
    CHECK ((transaction_id IS NULL) = (chosen_at IS NULL))
) STRICT;
