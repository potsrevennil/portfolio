//! The fetch's store: what the ledger needs, read from the ledger, and what
//! the cache holds and was asked, read from and written to market.db.

use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use ledger_types::currency::Currency;
use prices::{
    rates::{Need, RateStore},
    span::{union, Span},
    PriceStore, StockPrice,
};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use super::{Market, RATES_JOB};

/// The fetch's view of both databases: what the ledger holds, and what the
/// cache has.
#[derive(Clone, Debug)]
pub struct Rates {
    pub ledger: SqlitePool,
    pub market: Market,
}

impl RateStore for Rates {
    async fn needs(&self, base: Currency) -> Result<Vec<Need>> {
        let rows: Vec<(String, NaiveDate)> = sqlx::query_as(
            "SELECT p.currency, MIN(t.date) FROM postings p JOIN transactions t ON t.id = \
             p.transaction_id WHERE t.deleted_at IS NULL GROUP BY p.currency ORDER BY p.currency",
        )
        .fetch_all(&self.ledger)
        .await?;
        // A security symbol is not a currency; held securities join later.
        Ok(rows
            .into_iter()
            .filter_map(|(code, since)| code.parse().ok().map(|currency| Need { currency, since }))
            .filter(|n| n.currency != base)
            .collect())
    }

    async fn quote_dates(&self, tickers: &[String]) -> Result<HashMap<String, Vec<NaiveDate>>> {
        let mut dates: HashMap<String, Vec<NaiveDate>> = HashMap::new();
        for chunk in tickers.chunks(500) {
            let mut query: QueryBuilder<Sqlite> =
                QueryBuilder::new("SELECT symbol, date FROM stock_prices WHERE symbol IN (");
            let mut list = query.separated(", ");
            for ticker in chunk {
                list.push_bind(ticker);
            }
            query.push(") ORDER BY symbol, date");
            let rows: Vec<(String, NaiveDate)> =
                query.build_query_as().fetch_all(self.market.pool()).await?;
            for (symbol, date) in rows {
                dates.entry(symbol).or_default().push(date);
            }
        }
        Ok(dates)
    }

    async fn asked(&self, base: Currency) -> Result<HashMap<Currency, Vec<Span>>> {
        let rows: Vec<(String, NaiveDate, NaiveDate)> = sqlx::query_as(
            "SELECT currency, from_date, through_date FROM rate_asked WHERE base = ? ORDER BY \
             currency, from_date",
        )
        .bind(base.to_string())
        .fetch_all(self.market.pool())
        .await?;
        let mut asked: HashMap<Currency, Vec<Span>> = HashMap::new();
        for (currency, from, through) in rows {
            asked.entry(currency.parse()?).or_default().push(Span::new(from, through));
        }
        Ok(asked)
    }

    async fn save_quotes(&self, ticker: &str, quotes: &[StockPrice]) -> Result<()> {
        self.market
            .prices()
            .save_stock_prices(&HashMap::from([(ticker.into(), quotes.to_vec())]))
            .await
    }

    async fn record_asked(&self, currency: Currency, base: Currency, span: Span) -> Result<()> {
        let (currency, base) = (currency.to_string(), base.to_string());
        let mut tx = self.market.pool().begin().await?;
        let held: Vec<(NaiveDate, NaiveDate)> = sqlx::query_as(
            "SELECT from_date, through_date FROM rate_asked WHERE currency = ? AND base = ?",
        )
        .bind(&currency)
        .bind(&base)
        .fetch_all(&mut *tx)
        .await?;
        let merged = union(held.into_iter().map(|(f, t)| Span::new(f, t)).chain([span]));
        sqlx::query("DELETE FROM rate_asked WHERE currency = ? AND base = ?")
            .bind(&currency)
            .bind(&base)
            .execute(&mut *tx)
            .await?;
        for span in merged {
            sqlx::query(
                "INSERT INTO rate_asked (currency, base, from_date, through_date) VALUES (?, ?, \
                 ?, ?)",
            )
            .bind(&currency)
            .bind(&base)
            .bind(span.from)
            .bind(span.through)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn record_run(&self, at: DateTime<Utc>, error: Option<&str>) -> Result<()> {
        let at = at.to_rfc3339();
        sqlx::query(
            "INSERT INTO fetch_status (job, last_attempt, last_success, last_error) VALUES (?3, \
             ?1, CASE WHEN ?2 IS NULL THEN ?1 END, ?2) ON CONFLICT (job) DO UPDATE SET \
             last_attempt = excluded.last_attempt, last_success = COALESCE(excluded.last_success, \
             fetch_status.last_success), last_error = excluded.last_error",
        )
        .bind(at)
        .bind(error)
        .bind(RATES_JOB)
        .execute(self.market.pool())
        .await?;
        Ok(())
    }
}
