use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::NaiveDate;
use csv;

use crate::{
    portfolio::portfolio::{CsvTransactionRecord, Event, Security},
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

pub async fn store_transactions(
    events: &BTreeMap<NaiveDate, Event>,
    securities: &HashMap<String, Security>,
    output_file: Option<String>,
) -> Result<()> {
    // Write transactions to CSV if path is provided
    if let Some(file_path) = output_file {
        let mut writer = csv::Writer::from_path(file_path)?;
        for event in events.values() {
            let mut transactions = event.transactions.clone();
            transactions.sort_by_key(|t| t.datetime);

            for t in &transactions {
                let description =
                    securities.get(&t.symbol).map_or(String::new(), |s| s.description.clone());
                writer.serialize(CsvTransactionRecord {
                    id: t.id.clone(),
                    source: t.source,
                    asset_class: t.asset_class,
                    symbol: t.symbol.clone(),
                    description,
                    kind: t.kind,
                    datetime: t.datetime,
                    settle_date: t.settle_date,
                    quantity: t.quantity,
                    price: t.price,
                    amount: t.amount,
                    commission: t.commission,
                    currency: t.currency,
                    balance: t.balance,
                })?;
            }
        }
        writer.flush()?;
    }

    Ok(())
}

pub async fn load_splits(split_store: &SplitStore) -> Result<StockSplits> {
    let start_date = NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
    let end_date = NaiveDate::from_ymd_opt(2100, 12, 31).unwrap();
    split_store.get_splits(start_date, end_date).await
}
