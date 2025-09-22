use anyhow::{Context, Result};
use chrono::NaiveDate;
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
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
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
pub struct YFinanceSource;

impl YFinanceSource {
    pub async fn fetch_stock_prices(
        symbol: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<Vec<StockPrice>, PriceError> {
        let client = YfClient::default();
        let ticker = Ticker::new(&client, symbol.to_string());

        let start_datetime = start_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let end_datetime = end_date.and_hms_opt(23, 59, 59).unwrap().and_utc();

        let history =
            ticker.history_builder().between(start_datetime, end_datetime).fetch().await
            .context(format!("Failed to fetch prices for {} from {} to {}", symbol, start_date, end_date))?;

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
