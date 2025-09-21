use anyhow::Result;
use chrono::NaiveDate;
use sqlx::query;
use thiserror::Error;
use yfinance_rs::{core::conversions, Ticker, YfClient, YfError};

// --- Custom Error Type ---
#[derive(Error, Debug)]
pub enum PriceError {
    #[error("Yahoo Finance error: {0}")]
    YFinance(#[from] YfError),
    #[error("SQLx error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("Other error: {0}")]
    Other(String),
}

impl From<String> for PriceError {
    fn from(err: String) -> Self { PriceError::Other(err) }
}

// --- Common Data Structures ---
#[derive(Debug, Clone, PartialEq)]
pub struct StockPrice {
    pub date: NaiveDate,
    pub close_price: f64,
}

// --- Layer 1: Yahoo Finance Data Source ---
pub struct YFinanceSource(YfClient);

impl YFinanceSource {
    pub fn new() -> Self { YFinanceSource(YfClient::default()) }

    pub async fn fetch_stock_prices(
        &self,
        symbol: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<Vec<StockPrice>, PriceError> {
        let ticker = Ticker::new(&self.0, symbol.to_string());

        let start_datetime = start_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let end_datetime = end_date.and_hms_opt(23, 59, 59).unwrap().and_utc();

        let history =
            ticker.history_builder().between(start_datetime, end_datetime).fetch().await?;

        let stock_prices = history
            .into_iter()
            .filter_map(|bar| {
                let timestamp = bar.ts;
                let close_price_money = bar.close;

                let date = timestamp.date_naive();

                let close_price = conversions::money_to_f64(&close_price_money);

                Some(StockPrice { date, close_price })
            })
            .collect();

        Ok(stock_prices)
    }
}

// --- Layer 2: Local Price PriceStore ---
pub struct StockPriceStore {
    pool: sqlx::SqlitePool,
}

impl StockPriceStore {
    pub fn new(pool: sqlx::SqlitePool) -> Self { StockPriceStore { pool } }

    pub async fn save_stock_prices(
        &self,
        symbol: &str,
        prices: &[StockPrice],
    ) -> Result<(), PriceError> {
        let mut tx = self.pool.begin().await?;
        for price in prices {
            query!(
                r#"
                INSERT OR REPLACE INTO stock_prices (symbol, date, close_price)
                VALUES (?, ?, ?)
                "#,
                symbol,
                price.date,
                price.close_price
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_latest_date(&self, symbol: &str) -> Result<Option<NaiveDate>, PriceError> {
        let record = query!(
            r#"
            SELECT MAX(date) as "max_date: NaiveDate" FROM stock_prices WHERE symbol = ?
            "#,
            symbol
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(record.map(|r| r.max_date).flatten())
    }

    pub async fn get_stock_prices_in_range(
        &self,
        symbol: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<Vec<StockPrice>, PriceError> {
        let records = query!(
            r#"
            SELECT date, close_price FROM stock_prices
            WHERE symbol = ? AND date >= ? AND date <= ?
            ORDER BY date ASC
            "#,
            symbol,
            start_date,
            end_date
        )
        .fetch_all(&self.pool)
        .await?;

        let stock_prices = records
            .into_iter()
            .map(|record| StockPrice { date: record.date, close_price: record.close_price })
            .collect();

        Ok(stock_prices)
    }
}
