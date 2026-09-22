use std::{collections::BTreeMap, future::Future};

use anyhow::Result;
use chrono::NaiveDate;
use rust_decimal::Decimal;

pub type StockSplits = BTreeMap<NaiveDate, Vec<(String, Decimal)>>;

/// Where splits are kept between runs. The database implements it; this crate
/// stays free of SQL.
pub trait SplitStore {
    fn save_splits(&self, splits: &StockSplits) -> impl Future<Output = Result<()>> + Send;

    fn get_splits(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> impl Future<Output = Result<StockSplits>> + Send;
}
