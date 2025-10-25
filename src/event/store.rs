use std::collections::BTreeMap;

use anyhow::Result;
use chrono::NaiveDate;

use crate::{
    portfolio::portfolio::Event,
    split::{service::StockSplits, store::SplitStore},
};

pub async fn store_splits(
    events: &BTreeMap<NaiveDate, Event>,
    split_store: &SplitStore,
) -> Result<()> {
    // Extract all splits from events and save them
    let mut all_splits: StockSplits = BTreeMap::new();
    for (date, event) in events {
        if !event.splits.is_empty() {
            all_splits.entry(*date).or_default().extend(event.splits.clone());
        }
    }
    split_store.save_splits(&all_splits).await?;
    Ok(())
}

pub async fn load_splits(split_store: &SplitStore) -> Result<StockSplits> {
    let start_date = NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
    let end_date = NaiveDate::from_ymd_opt(2100, 12, 31).unwrap();
    split_store.get_splits(start_date, end_date).await
}
