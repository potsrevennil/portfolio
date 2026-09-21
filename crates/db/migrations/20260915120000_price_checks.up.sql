-- How far through the source has been asked for each symbol, so a repeat run
-- the same day does not re-probe for data already fetched.
CREATE TABLE IF NOT EXISTS price_checks (
    symbol TEXT PRIMARY KEY,
    fetched_through DATE NOT NULL
);
