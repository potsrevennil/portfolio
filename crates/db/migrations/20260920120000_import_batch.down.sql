DROP INDEX IF EXISTS idx_transactions_import_batch;
ALTER TABLE transactions DROP COLUMN import_batch_id;
DROP TABLE IF EXISTS import_batch;
