-- T05: what stopped an import, kept after its rollback so the review queue can
-- list it until a person has fixed the cause and the import runs again.
--
-- A statement line that fits several unverified records stays in
-- verification_choice, where a person picks; every other stop is a row here.
-- `problem` is the typed stop as JSON. Each run of an importer clears what it
-- left open: a refused run replaces it with what it met, a successful one
-- leaves nothing.
CREATE TABLE import_stop (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    source     TEXT NOT NULL,                      -- the importer, as import_batch.source names it
    kind       TEXT NOT NULL                       -- a db::stops::Stop variant, as strum names it
        CHECK (kind IN ('record_ambiguous', 'record_unexplained', 'not_on_statement',
                        'shared_deferral', 'two_dates', 'several_openings', 'unchained')),
    subject    TEXT NOT NULL,                      -- what it is about: a record's ref, a transaction id
    problem    TEXT NOT NULL CHECK (json_valid(problem)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    cleared_at TEXT,
    UNIQUE (source, kind, subject)
) STRICT;
