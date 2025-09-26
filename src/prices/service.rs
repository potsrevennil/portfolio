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
}

impl PriceService {
    /// Creates a new `PriceService` instance.
    pub fn new(store: StockPriceStore) -> Self { Self { store } }

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
        let mut fetch_futures = Vec::new();

        // Retrieve existing prices from the database for the requested range.
        let mut all_prices =
            self.store.get_stock_prices_in_range(symbols, start_date, end_date).await?;

        // For each symbol, determine if additional data needs to be fetched from the
        // web.
        for &symbol in symbols {
            let needs_fetching = all_prices.get(symbol).is_none_or(|prices| {
                prices.is_empty()
                    || prices.first().unwrap().date > start_date
                    || prices.last().unwrap().date < end_date
            });

            if needs_fetching {
                let symbol_owned = symbol.to_string();
                // Prepare an asynchronous task to fetch missing data for this symbol.
                fetch_futures.push(async move {
                    let prices =
                        YFinanceSource::fetch_stock_prices(&symbol_owned, start_date, end_date)
                            .await?;
                    Ok::<(String, Vec<StockPrice>), PriceError>((symbol_owned, prices))
                });
            }
        }

        // Execute all pending data fetching tasks concurrently.
        let mut fetched_results: HashMap<String, Vec<StockPrice>> =
            try_join_all(fetch_futures).await?.into_iter().collect();

        fetched_results.retain(|_, v| !v.is_empty());

        // Save any newly fetched data to the database.
        if !fetched_results.is_empty() {
            self.store.save_stock_prices(&fetched_results).await?;
        }

        // Merge the newly fetched data with the data already retrieved from the
        // database.
        all_prices.extend(fetched_results);

        Ok(all_prices)
    }
}
