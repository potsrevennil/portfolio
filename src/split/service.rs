use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::split::store::SplitStore;

pub type StockSplits = BTreeMap<NaiveDate, Vec<(String, Decimal)>>;

pub struct Splits {
    store: SplitStore,
}

impl Splits {
    pub fn new(store: SplitStore) -> Self { Splits { store } }

    pub async fn save(&self, splits: &StockSplits) -> Result<()> {
        self.store.save_splits(splits).await
    }

    pub async fn get(&self, start: NaiveDate, end: NaiveDate) -> Result<StockSplits> {
        self.store.get_splits(start, end).await
    }
}
