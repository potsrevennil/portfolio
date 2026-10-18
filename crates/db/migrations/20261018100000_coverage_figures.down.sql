DROP VIEW assertion_coverage;

CREATE VIEW assertion_coverage AS
    SELECT account_id, currency AS commodity, period_end AS as_of, source, 'balance' AS kind
    FROM balance_assertion
    UNION ALL
    SELECT account_id, commodity, as_of, source, kind
    FROM holding_assertion;
