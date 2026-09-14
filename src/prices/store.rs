use std::collections::HashMap;

use anyhow::Result;
use chrono::NaiveDate;
use sqlx::{QueryBuilder, Sqlite};

use super::source::{PriceError, StockPrice};

#[derive(sqlx::FromRow, Debug)]
struct StockPriceRow {
    symbol: String,
    date: NaiveDate,
    close_price: f64,
}

#[derive(sqlx::FromRow, Debug)]
struct PriceCheckRow {
    symbol: String,
    fetched_through: NaiveDate,
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
        let mut all_prices_to_save: Vec<(String, NaiveDate, f64)> = Vec::new();
        for (symbol, prices) in prices_map.iter() {
            for price in prices {
                all_prices_to_save.push((symbol.clone(), price.date, price.close_price));
            }
        }

        // SQLite has a limit of 999 host parameters. Each price uses 3 parameters.
        // So, max_prices_per_batch = 999 / 3 = 333.
        // We'll use a slightly smaller batch size to be safe.
        const BATCH_SIZE: usize = 300;

        for chunk in all_prices_to_save.chunks(BATCH_SIZE) {
            let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
                "INSERT OR REPLACE INTO stock_prices (symbol, date, close_price) ",
            );

            query_builder.push_values(chunk.iter(), |mut b, (symbol, date, close_price)| {
                b.push_bind(symbol).push_bind(date).push_bind(close_price);
            });

            let query = query_builder.build();
            query.execute(&self.pool).await?;
        }

        Ok(())
    }

    /// The date each symbol was last fetched through, for symbols that have a
    /// record. A symbol not in the map has never been fetched.
    pub async fn get_fetched_through(
        &self,
        symbols: &[&str],
    ) -> Result<HashMap<String, NaiveDate>, PriceError> {
        let mut fetched_through = HashMap::new();
        const BATCH_SIZE: usize = 500;

        for chunk in symbols.chunks(BATCH_SIZE) {
            let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
                "SELECT symbol, fetched_through FROM price_checks WHERE symbol IN (",
            );
            let mut separated = query_builder.separated(", ");
            for symbol in chunk {
                separated.push_bind(symbol);
            }
            separated.push_unseparated(")");

            let rows =
                query_builder.build_query_as::<PriceCheckRow>().fetch_all(&self.pool).await?;
            for row in rows {
                fetched_through.insert(row.symbol, row.fetched_through);
            }
        }

        Ok(fetched_through)
    }

    /// Record that these symbols have been fetched through `date`.
    pub async fn mark_fetched_through(
        &self,
        symbols: &[&str],
        date: NaiveDate,
    ) -> Result<(), PriceError> {
        if symbols.is_empty() {
            return Ok(());
        }
        // Two host parameters per row, well under SQLite's 999 limit.
        const BATCH_SIZE: usize = 400;

        for chunk in symbols.chunks(BATCH_SIZE) {
            let mut query_builder: QueryBuilder<Sqlite> =
                QueryBuilder::new("INSERT OR REPLACE INTO price_checks (symbol, fetched_through) ");
            query_builder.push_values(chunk.iter(), |mut b, symbol| {
                b.push_bind(*symbol).push_bind(date);
            });
            query_builder.build().execute(&self.pool).await?;
        }

        Ok(())
    }

    pub async fn get_latest_date(&self, symbol: &str) -> Result<Option<NaiveDate>, PriceError> {
        let record = sqlx::query!(
            r#"
            SELECT MAX(date) as "max_date: NaiveDate" FROM stock_prices WHERE symbol = ?
            "#,
            symbol
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(record.and_then(|r| r.max_date))
    }

    pub async fn get_stock_prices_in_range(
        &self,
        symbols: &[&str],
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<HashMap<String, Vec<StockPrice>>, PriceError> {
        let mut prices_map = HashMap::new();

        // SQLite has a limit of 999 host parameters. Each symbol uses 1 parameter.
        // We'll use a batch size of 500 to be safe.
        const BATCH_SIZE: usize = 500;

        for symbol_chunk in symbols.chunks(BATCH_SIZE) {
            let mut query_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
                "SELECT symbol, date, close_price FROM stock_prices WHERE symbol IN (",
            );

            let mut separated = query_builder.separated(", ");
            for symbol in symbol_chunk {
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

            for row in records {
                prices_map
                    .entry(row.symbol)
                    .or_insert_with(Vec::new)
                    .push(StockPrice { date: row.date, close_price: row.close_price });
            }
        }

        Ok(prices_map)
    }
}
