use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;
use rust_decimal::{
    prelude::{FromPrimitive, ToPrimitive},
    Decimal,
};
use sqlx::{query_as, SqlitePool};

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

        let mut all_splits_to_save: Vec<(String, NaiveDate, f64)> = Vec::new();
        for (date, splits_for_date) in splits {
            for (symbol, ratio) in splits_for_date {
                let ratio_f64 = ratio.to_f64().unwrap_or(1.0);
                all_splits_to_save.push((symbol.clone(), *date, ratio_f64));
            }
        }

        let mut tx = self.pool.begin().await?;

        // SQLite has a limit of 999 host parameters. Each split uses 3 parameters.
        // So, max_splits_per_batch = 999 / 3 = 333.
        // We'll use a slightly smaller batch size to be safe.
        const BATCH_SIZE: usize = 300;

        for chunk in all_splits_to_save.chunks(BATCH_SIZE) {
            let mut query_builder = String::from(
                "INSERT INTO stock_splits (symbol, date, ratio) VALUES ",
            );


            for (i, (_symbol, _date, _ratio)) in chunk.iter().enumerate() {
                if i > 0 {
                    query_builder.push_str(", ");
                }
                query_builder.push_str("(?, ?, ?)");

            }

            query_builder.push_str(" ON CONFLICT(symbol, date) DO UPDATE SET ratio = excluded.ratio;");

            let mut query_exec = sqlx::query(&query_builder);
            for (symbol, date, ratio) in chunk.iter() {
                query_exec = query_exec.bind(symbol);
                query_exec = query_exec.bind(date);
                query_exec = query_exec.bind(ratio);
            }
            query_exec.execute(&mut *tx).await?;
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
