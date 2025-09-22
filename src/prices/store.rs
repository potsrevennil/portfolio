use anyhow::Result;
use chrono::NaiveDate;
use sqlx::query;

use super::source::{PriceError, StockPrice};

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
