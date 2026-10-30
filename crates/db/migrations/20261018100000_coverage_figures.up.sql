-- The coverage page starts a bar where a statement's period does.
-- A superseded 天天記帳 closing is no longer checked, so it vouches for nothing.
DROP VIEW assertion_coverage;

CREATE VIEW assertion_coverage AS
    SELECT account_id, currency AS commodity, period_start, period_end AS as_of,
           source, 'balance' AS kind
    FROM balance_assertion
    WHERE superseded_at IS NULL
    UNION ALL
    SELECT account_id, commodity, NULL, as_of, source, kind
    FROM holding_assertion;
