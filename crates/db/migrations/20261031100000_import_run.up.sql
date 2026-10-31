-- T05: an import no longer refuses what it finds in the ledger. Each run keeps
-- every transaction it touched, with what it suggests a person looks at; a
-- row it suggests nothing for is confirmed.
CREATE TABLE import_run (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    source     TEXT NOT NULL,                      -- the importer, as import_batch.source names it
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE import_item (
    run_id         INTEGER NOT NULL REFERENCES import_run (id) ON DELETE CASCADE,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    suggestions    TEXT NOT NULL CHECK (json_valid(suggestions)),  -- db::import_run::Suggestion array
    PRIMARY KEY (run_id, transaction_id)
) STRICT;
CREATE INDEX import_item_transaction ON import_item (transaction_id);
