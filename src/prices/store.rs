use std::collections::HashMap;

use anyhow::Result;
use chrono::NaiveDate;
use sqlx::{query, QueryBuilder, Sqlite};

use super::source::{PriceError, StockPrice};

#[derive(sqlx::FromRow, Debug)]
struct StockPriceRow {
    symbol: String,
    date: NaiveDate,
    close_price: f64,
}

// --- Layer 2: Local Price PriceStore ---
#[derive(Clone)]
pub struct StockPriceStore {
    pool: sqlx::SqlitePool,
}

impl StockPriceStore {
    pub fn new(pool: sqlx::SqlitePool) -> Self { StockPriceStore { pool } }

    pub async fn save_stock_prices(
        &self,
        prices_map: &HashMap<String, Vec<StockPrice>>,
    ) -> Result<(), PriceError> {
        if prices_map.is_empty() {
            return Ok(());
        }
        let mut query_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new("INSERT OR REPLACE INTO stock_prices (symbol, date, close_price) ");

        query_builder.push_values(
            prices_map.iter().flat_map(|(symbol, prices)| {
                prices.iter().map(move |price| (symbol.as_str(), price.date, price.close_price))
            }),
            |mut b, (symbol, date, close_price)| {
                b.push_bind(symbol).push_bind(date).push_bind(close_price);
            },
        );

        let query = query_builder.build();
        query.execute(&self.pool).await?;

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
        symbols: &[&str],
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<HashMap<String, Vec<StockPrice>>, PriceError> {
        if symbols.is_empty() {
            return Ok(HashMap::new());
        }

        let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT symbol, date, close_price FROM stock_prices WHERE symbol IN (",
        );

        let mut separated = query_builder.separated(", ");
        for symbol in symbols {
            separated.push_bind(symbol);
        }
        separated.push_unseparated(") ");

        query_builder.push("AND date >= ");
        query_builder.push_bind(start_date);
        query_builder.push(" AND date <= ");
        query_builder.push_bind(end_date);
        query_builder.push(" ORDER BY symbol, date ASC");

        let query = query_builder.build_query_as::<StockPriceRow>();
        let records = query.fetch_all(&self.pool).await?;

        let mut prices_map = HashMap::new();
        for row in records {
            prices_map
                .entry(row.symbol)
                .or_insert_with(Vec::new)
                .push(StockPrice { date: row.date, close_price: row.close_price });
        }

        Ok(prices_map)
    }
}
