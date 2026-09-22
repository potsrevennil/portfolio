use chrono::NaiveDate;
use db::splits::StockSplitStore;
use portfolio::{SplitStore, StockSplits};
use rust_decimal_macros::dec;

#[path = "common/mod.rs"]
mod common;
use common::create_temp_db;

fn day(y: i32, m: u32, d: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, d).unwrap() }

/// Saving a split that is already stored replaces its ratio, and a read returns
/// only the splits inside the range.
#[tokio::test]
async fn splits_round_trip_and_upsert() -> anyhow::Result<()> {
    let (pool, _db_file) = create_temp_db().await?;
    let store = StockSplitStore::new(pool);

    store
        .save_splits(&StockSplits::from([(day(2020, 8, 31), vec![("AAPL".into(), dec!(3))])]))
        .await?;
    store
        .save_splits(&StockSplits::from([
            (day(2020, 8, 31), vec![("AAPL".into(), dec!(4))]),
            (day(2024, 6, 10), vec![("NVDA".into(), dec!(10))]),
        ]))
        .await?;

    let all = store.get_splits(day(2000, 1, 1), day(2030, 1, 1)).await?;
    assert_eq!(
        all,
        StockSplits::from([
            (day(2020, 8, 31), vec![("AAPL".into(), dec!(4))]),
            (day(2024, 6, 10), vec![("NVDA".into(), dec!(10))]),
        ])
    );

    let only_2024 = store.get_splits(day(2024, 1, 1), day(2024, 12, 31)).await?;
    assert_eq!(only_2024.keys().copied().collect::<Vec<_>>(), vec![day(2024, 6, 10)]);
    Ok(())
}
