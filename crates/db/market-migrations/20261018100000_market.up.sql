-- market.db: prices, FX and splits. A cache the fetcher can rebuild, kept
-- apart from the ledger's own records so it needs no backup.

CREATE TABLE stock_prices (
    symbol      TEXT NOT NULL,
    date        DATE NOT NULL,
    close_price REAL NOT NULL,
    fetched_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (symbol, date)
);

CREATE TABLE stock_splits (
    symbol TEXT NOT NULL,
    date   DATE NOT NULL,
    ratio  REAL NOT NULL,
    PRIMARY KEY (symbol, date)
);

-- How far the stock tracker has asked the source for each symbol.
CREATE TABLE price_checks (
    symbol          TEXT PRIMARY KEY,
    fetched_through DATE NOT NULL
);

-- Days already asked of the source for a currency's rate in `base`, whatever
-- it answered, so a pair with no history there is not asked again each run.
CREATE TABLE rate_asked (
    currency     TEXT NOT NULL,
    base         TEXT NOT NULL,
    from_date    DATE NOT NULL,
    through_date DATE NOT NULL,
    PRIMARY KEY (currency, base, from_date)
);

-- Each background job's last run, for the coverage page.
CREATE TABLE fetch_status (
    job          TEXT PRIMARY KEY,
    last_attempt TEXT NOT NULL,
    last_success TEXT,
    last_error   TEXT
);
