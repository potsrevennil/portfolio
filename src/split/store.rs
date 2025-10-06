use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;
use rust_decimal::{
    prelude::{FromPrimitive, ToPrimitive},
    Decimal,
};
use sqlx::{query, query_as, SqlitePool};

use crate::split::service::StockSplits;

#[derive(sqlx::FromRow, Debug)]
struct StockSplitRow {
    symbol: String,
    date: NaiveDate,
    ratio: f64,
}

#[derive(Clone)]
pub struct SplitStore {
    pool: SqlitePool,
}

impl SplitStore {
    pub fn new(pool: SqlitePool) -> Self { SplitStore { pool } }

    pub async fn save_splits(&self, splits: &StockSplits) -> Result<()> {
        if splits.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;

        for (symbol, splits) in splits {
            for (date, ratio) in splits {
                let ratio_f64 = ratio.to_f64().unwrap_or(1.0);
                query!(
                    r#"
                    INSERT INTO stock_splits (symbol, date, ratio)
                    VALUES (?, ?, ?)
                    ON CONFLICT(symbol, date) DO UPDATE SET
                        ratio = excluded.ratio;
                    "#,
                    symbol,
                    *date,
                    ratio_f64
                )
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn get_splits(
        &self,
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<StockSplits> {
        let records = query_as!(
            StockSplitRow,
            "SELECT symbol, date, ratio FROM stock_splits WHERE date >= ? AND date <= ? ORDER BY \
             symbol, date ASC",
            start_date,
            end_date
        )
        .fetch_all(&self.pool)
        .await?;

        let mut splits: StockSplits = BTreeMap::new();
        for row in records {
            splits
                .entry(row.date)
                .or_default()
                .push((row.symbol, Decimal::from_f64(row.ratio).unwrap_or(Decimal::ONE)));
        }
        Ok(splits)
    }
}
