use std::collections::HashMap;

use chrono::NaiveDate;
use portfolio::prices::{StockPrice, StockPriceStore};
use tokio;

#[path = "../common/mod.rs"]
mod common;
use common::create_temp_db;

#[tokio::test]
async fn test_save_and_get_stock_prices() -> anyhow::Result<()> {
    let (pool, _db_file) = create_temp_db().await?;
    let price_store = StockPriceStore::new(pool.clone());

    // 1. Save initial data
    let mut prices_map1 = HashMap::new();
    prices_map1.insert("AAPL".to_string(), vec![
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 1).unwrap(), close_price: 100.0 },
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 2).unwrap(), close_price: 101.0 },
    ]);
    price_store.save_stock_prices(&prices_map1).await?;

    // 2. Save again with updated and new data
    let mut prices_map2 = HashMap::new();
    prices_map2.insert("AAPL".to_string(), vec![
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 2).unwrap(), close_price: 101.5 }, // Update
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 3).unwrap(), close_price: 102.0 }, // New
    ]);
    prices_map2.insert("GOOG".to_string(), vec![
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 1).unwrap(), close_price: 200.0 }, // New symbol
    ]);
    price_store.save_stock_prices(&prices_map2).await?;

    // 3. Verify the data
    let symbols = vec!["AAPL", "GOOG"];
    let start_date = NaiveDate::from_ymd_opt(2023, 1, 1).unwrap();
    let end_date = NaiveDate::from_ymd_opt(2023, 1, 3).unwrap();
    let result = price_store.get_stock_prices_in_range(&symbols, start_date, end_date).await?;

    assert_eq!(result.len(), 2);

    let aapl_prices = result.get("AAPL").unwrap();
    assert_eq!(aapl_prices.len(), 3);
    assert_eq!(aapl_prices[0].date, NaiveDate::from_ymd_opt(2023, 1, 1).unwrap());
    assert_eq!(aapl_prices[0].close_price, 100.0);
    assert_eq!(aapl_prices[1].date, NaiveDate::from_ymd_opt(2023, 1, 2).unwrap());
    assert_eq!(aapl_prices[1].close_price, 101.5); // Updated value
    assert_eq!(aapl_prices[2].date, NaiveDate::from_ymd_opt(2023, 1, 3).unwrap());
    assert_eq!(aapl_prices[2].close_price, 102.0);

    let goog_prices = result.get("GOOG").unwrap();
    assert_eq!(goog_prices.len(), 1);
    assert_eq!(goog_prices[0].date, NaiveDate::from_ymd_opt(2023, 1, 1).unwrap());
    assert_eq!(goog_prices[0].close_price, 200.0);

    Ok(())
}
