DROP TABLE IF EXISTS verification_choice;
DROP TRIGGER IF EXISTS transaction_events_no_delete;
DROP TRIGGER IF EXISTS transaction_events_no_update;
DROP INDEX IF EXISTS idx_transaction_events_transaction;
DROP TABLE IF EXISTS transaction_events;
ALTER TABLE postings DROP COLUMN origin;
