use std::collections::HashMap;

use chrono::NaiveDate;
use db::quotes::StockPriceStore;
use prices::{PriceService, PriceStore, StockPrice};

#[path = "common/mod.rs"]
mod common;
use common::create_temp_db;

fn day(y: i32, m: u32, d: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, d).unwrap() }

/// A cache that reaches the start but stops short of the requested end is
/// extended by fetching only the tail past its last day, then merged — kept
/// sorted and without duplicates — with what was already stored.
#[tokio::test]
#[ignore = "fetches from Yahoo Finance"]
async fn get_prices_fetches_the_missing_tail() -> anyhow::Result<()> {
    let (pool, _db_file) = create_temp_db().await?;
    let price_store = StockPriceStore::new(pool.clone());
    let price_service = PriceService::new(price_store.clone());

    let mut cached = HashMap::new();
    cached.insert("AAPL".to_string(), vec![
        StockPrice { date: day(2023, 1, 3), close_price: 100.0 },
        StockPrice { date: day(2023, 1, 4), close_price: 101.0 },
    ]);
    price_store.save_stock_prices(&cached).await?;

    // 2023-01-05 and -06 are the missing tail (both trading days).
    let prices =
        price_service.get_prices(&["AAPL"], day(2023, 1, 3), day(2023, 1, 6), false).await?;

    let aapl = prices.get("AAPL").expect("AAPL present");
    for date in [day(2023, 1, 3), day(2023, 1, 4), day(2023, 1, 5), day(2023, 1, 6)] {
        assert!(aapl.iter().any(|p| p.date == date), "missing {date}");
    }
    assert!(aapl.windows(2).all(|w| w[0].date < w[1].date), "not sorted / has duplicates");
    Ok(())
}

/// The fetched-through marker round-trips: absent until set, then readable, and
/// only for the symbols that were marked.
#[tokio::test]
async fn fetched_through_round_trips() -> anyhow::Result<()> {
    let (pool, _db_file) = create_temp_db().await?;
    let store = StockPriceStore::new(pool);

    assert!(store.get_fetched_through(&["AAPL"]).await?.is_empty());

    let through = day(2024, 5, 1);
    store.mark_fetched_through(&["AAPL", "MSFT"], through).await?;

    let got = store.get_fetched_through(&["AAPL", "GOOG"]).await?;
    assert_eq!(got.get("AAPL"), Some(&through), "marked symbol not read back");
    assert_eq!(got.get("GOOG"), None, "unmarked symbol should be absent");
    Ok(())
}
