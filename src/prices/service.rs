use std::collections::HashMap;

use anyhow::Result;
use chrono::NaiveDate;
use futures::future::try_join_all;

use super::{
    source::{PriceError, StockPrice, YFinanceSource},
    store::StockPriceStore,
};

/// Manages stock price data, acting as an intermediary between the application
/// and data storage/fetching mechanisms.
pub struct PriceService {
    store: StockPriceStore,
    source: YFinanceSource,
}

impl PriceService {
    /// Creates a new `PriceService` instance.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { store: StockPriceStore::new(pool), source: YFinanceSource::new() }
    }

    /// Retrieves stock prices for specified symbols within a given date range.
    ///
    /// Fetches data from the local database first. If data is missing or
    /// incomplete, it fetches from an external web source, saves it, and
    /// returns the complete data.
    ///
    /// # Arguments
    /// * `symbols` - List of stock ticker symbols.
    /// * `start_date` - Beginning of the date range (inclusive).
    /// * `end_date` - End of the date range (inclusive).
    ///
    /// # Returns
    /// A `HashMap` where keys are stock symbols and values are lists of
    /// `StockPrice` objects. Returns an error if data fetching or storage
    /// fails.
    pub async fn get_prices(
        &self,
        symbols: &[&str],
        start_date: NaiveDate,
        end_date: NaiveDate,
    ) -> Result<HashMap<String, Vec<StockPrice>>, PriceError> {
        // Existing prices for the requested range, ordered by date per symbol.
        let mut all_prices =
            self.store.get_stock_prices_in_range(symbols, start_date, end_date).await?;

        // Fetch only what the cache is missing: the tail past the last cached
        // day — the usual case, since `end_date` advances daily — or the whole
        // range when nothing is cached or the cache does not reach `start_date`.
        // Re-fetching the full range each run re-downloaded years of history.
        let mut fetch_futures = Vec::new();
        for &symbol in symbols {
            let fetch_start = match all_prices.get(symbol) {
                Some(prices) if prices.first().is_some_and(|p| p.date <= start_date) => {
                    let last = prices.last().unwrap().date;
                    if last >= end_date {
                        continue; // already covered
                    }
                    last.succ_opt().unwrap_or(end_date)
                }
                _ => start_date,
            };
            let symbol_owned = symbol.to_string();
            fetch_futures.push(async move {
                // A single symbol's error is swallowed so one bad symbol does
                // not fail the batch. Logged at debug, not warn: a symbol with
                // no data is an expected, benign outcome here — callers probe
                // FX pairs that may not exist — and the caller warns if a
                // missing series actually matters.
                match self.source.fetch_stock_prices(&symbol_owned, fetch_start, end_date).await {
                    Ok(prices) => {
                        Ok::<(String, Vec<StockPrice>), PriceError>((symbol_owned, prices))
                    }
                    Err(e) => {
                        log::debug!("no prices for {}: {}", symbol_owned, e);
                        Ok((symbol_owned, Vec::new()))
                    }
                }
            });
        }

        // Execute all pending data fetching tasks concurrently.
        let fetched_results: HashMap<String, Vec<StockPrice>> = try_join_all(fetch_futures)
            .await?
            .into_iter()
            .filter(|(_, prices)| !prices.is_empty())
            .collect();

        // Save any newly fetched data to the database.
        if !fetched_results.is_empty() {
            self.store.save_stock_prices(&fetched_results).await?;
        }

        // Merge each fetched delta onto the cached series. A delta abuts the
        // cache, but a full re-fetch overlaps it, so dedup by date.
        for (symbol, prices) in fetched_results {
            let series = all_prices.entry(symbol).or_default();
            series.extend(prices);
            series.sort_by_key(|p| p.date);
            series.dedup_by_key(|p| p.date);
        }

        Ok(all_prices)
    }
}
