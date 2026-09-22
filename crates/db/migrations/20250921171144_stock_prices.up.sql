-- Add up migration script here
CREATE TABLE IF NOT EXISTS stock_prices (
    symbol TEXT NOT NULL,
    date DATE NOT NULL,
    close_price REAL NOT NULL,
    fetched_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (symbol, date)
);
