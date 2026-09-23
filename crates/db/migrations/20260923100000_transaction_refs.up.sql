-- Further source records a transaction stands for besides its external_ref,
-- e.g. the statement line that verified a 天天記帳 record, or the 天天記帳
-- record paired with an imported line. Dedup reads both.
CREATE TABLE transaction_refs (
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    external_ref   TEXT NOT NULL UNIQUE
) STRICT;

CREATE INDEX idx_transaction_refs_transaction ON transaction_refs (transaction_id);
