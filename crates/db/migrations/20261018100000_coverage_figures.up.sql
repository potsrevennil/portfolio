-- The coverage page needs each figure's period, to find gaps between
-- statements, and the figure itself: a zero leaves nothing to check later.
-- A superseded 天天記帳 closing is no longer checked, so it vouches for nothing.
DROP VIEW assertion_coverage;

CREATE VIEW assertion_coverage AS
    SELECT account_id, currency AS commodity, period_start, period_end AS as_of,
           closing AS figure, source, 'balance' AS kind
    FROM balance_assertion
    WHERE superseded_at IS NULL
    UNION ALL
    SELECT account_id, commodity, NULL, as_of, quantity, source, kind
    FROM holding_assertion;
