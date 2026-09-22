use std::{collections::HashMap, future::Future};

use anyhow::Result;
use chrono::NaiveDate;

use super::source::StockPrice;

/// Where fetched quotes are kept, so a run fetches only what is missing. The
/// database implements it; this crate stays free of SQL.
pub trait PriceStore {
    fn save_stock_prices(
        &self,
        prices: &HashMap<String, Vec<StockPrice>>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// The date each symbol was last fetched through. A symbol not in the map
    /// has never been fetched.
    fn get_fetched_through(
        &self,
        symbols: &[&str],
    ) -> impl Future<Output = Result<HashMap<String, NaiveDate>>> + Send;

    fn mark_fetched_through(
        &self,
        symbols: &[&str],
        date: NaiveDate,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Each symbol's quotes in `[start, end]`, by ascending date.
    fn get_stock_prices_in_range(
        &self,
        symbols: &[&str],
        start: NaiveDate,
        end: NaiveDate,
    ) -> impl Future<Output = Result<HashMap<String, Vec<StockPrice>>>> + Send;
}
