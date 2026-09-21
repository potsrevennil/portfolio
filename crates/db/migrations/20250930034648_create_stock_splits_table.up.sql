-- Add up migration script here
CREATE TABLE stock_splits (
    symbol TEXT NOT NULL,
    date DATE NOT NULL,
    ratio REAL NOT NULL,
    PRIMARY KEY (symbol, date)
);