use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::prelude::FromPrimitive;
use thiserror::Error;
use yfinance_rs::{core::conversions, Ticker, YfClient, YfError};

use crate::portfolio::portfolio::Currency;

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
pub struct YFinanceSource {
    client: YfClient,
}

impl Default for YFinanceSource {
    fn default() -> Self { Self::new() }
}

impl YFinanceSource {
    pub fn new() -> Self { Self { client: YfClient::default() } }

    pub fn get_conversion_rate(
        from_currency: Currency,
        to_currency: Currency,
        prices: &std::collections::HashMap<String, Vec<StockPrice>>,
    ) -> rust_decimal::Decimal {
        if from_currency == to_currency {
            return rust_decimal::Decimal::ONE;
        }

        if let Some(ticker) = YFinanceSource::get_exchange_rate_ticker(from_currency, to_currency) {
            if let Some(exchange_prices) = prices.get(&ticker) {
                if let Some(latest_price) = exchange_prices.last() {
                    return rust_decimal::Decimal::from_f64(latest_price.close_price)
                        .unwrap_or_default();
                }
            }
        }
        rust_decimal::Decimal::ONE // Default to 1 if no conversion rate found
    }

    pub fn get_exchange_rate_ticker(
        from_currency: Currency,
        to_currency: Currency,
    ) -> Option<String> {
        if from_currency == to_currency {
            return None;
        }

        match (from_currency, to_currency) {
            (Currency::USD, to) => Some(format!("{}=X", to)),
            (from, Currency::USD) => Some(format!("{}USD=X", from)),
            (from, to) => Some(format!("{}{}=X", from, to)),
        }
    }

    pub async fn fetch_stock_prices(
        &self,
        symbol: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<Vec<StockPrice>, PriceError> {
        let ticker = Ticker::new(&self.client, symbol.to_string());

        let start_datetime = start_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let end_datetime = end_date.and_hms_opt(23, 59, 59).unwrap().and_utc();
        let history =
            ticker.history_builder().between(start_datetime, end_datetime).fetch().await.context(
                format!(
                    "Failed to fetch prices for {} from {} to {}",
                    symbol, start_date, end_date
                ),
            )?;

        let stock_prices: Vec<StockPrice> = history
            .into_iter()
            .map(|bar| {
                let timestamp = bar.ts;
                let close_price_money = bar.close;

                let date = timestamp.date_naive();

                let close_price = conversions::money_to_f64(&close_price_money);

                StockPrice { date, close_price }
            })
            .collect();
        Ok(stock_prices)
    }
}
