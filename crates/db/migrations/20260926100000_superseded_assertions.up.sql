-- A 天天記帳 closing is that app's own sum of its records, not an outside
-- fact. When a person later adds or edits a record on or before it (one
-- they forgot to log there), the closing stops being true; it is kept, marked
-- with when it was superseded, and no longer checked.
ALTER TABLE balance_assertion ADD COLUMN superseded_at TEXT;
