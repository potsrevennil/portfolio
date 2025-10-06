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

// Manual split data for Cathay or other brokers that don't provide this data
pub fn fixed_splits() -> StockSplits {
    let mut map = BTreeMap::new();
    // Example entry: IBKR 4-for-1 split on 2024-07-01
    // Note: This is a placeholder based on the user's example.
    // The actual symbol and date might need to be adjusted.
    map.insert(NaiveDate::from_ymd_opt(2024, 7, 1).unwrap(), vec![(
        "ZZ01.TW".to_string(),
        Decimal::from(4),
    )]);
    map
}
