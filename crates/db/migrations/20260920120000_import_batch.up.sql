-- Provenance of imported transactions: which importer read which file, when.
-- Statement balances are not kept here; they are balance assertions.
CREATE TABLE import_batch (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source      TEXT NOT NULL,                                 -- importer, e.g. 'cathay-bank'
    file        TEXT NOT NULL,
    imported_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

-- NULL for history loaded from the journal and for hand-entered rows.
ALTER TABLE transactions ADD COLUMN import_batch_id INTEGER REFERENCES import_batch (id);

CREATE INDEX idx_transactions_import_batch ON transactions (import_batch_id);
