use std::collections::HashMap;

use chrono::NaiveDate;
use portfolio::prices::{PriceService, StockPrice, StockPriceStore};
use tokio;

#[path = "../common/mod.rs"]
mod common;
use common::create_temp_db;

#[tokio::test]
async fn test_get_prices_fetches_missing_data() -> anyhow::Result<()> {
    let (pool, _db_file) = create_temp_db().await?;
    let price_store = StockPriceStore::new(pool.clone());
    let price_service = PriceService::new(pool.clone());

    // 1. Pre-populate the DB with a gap
    let symbol = "AAPL";
    let existing_prices = vec![
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 3).unwrap(), close_price: 100.0 },
        // Gap on 2023-01-04
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 5).unwrap(), close_price: 102.0 },
    ];
    let mut prices_map = HashMap::new();
    prices_map.insert(symbol.to_string(), existing_prices);
    price_store.save_stock_prices(&prices_map).await?;

    // 2. Call get_prices for a range that covers the gap
    let symbols = vec!["AAPL"];
    let start_date = NaiveDate::from_ymd_opt(2023, 1, 3).unwrap();
    let end_date = NaiveDate::from_ymd_opt(2023, 1, 6).unwrap();

    let prices = price_service.get_prices(&symbols, start_date, end_date).await?;

    // 3. Assert the results
    assert_eq!(prices.len(), 1);
    let aapl_prices = prices.get("AAPL").unwrap();

    // yfinance will not return data for weekends/holidays, so the exact number
    // might vary. Let's check for the known dates and that the data is sorted.
    assert!(aapl_prices.len() >= 2); // At least the two we put in

    // Check that the gap is filled
    assert!(aapl_prices.iter().any(|p| p.date == NaiveDate::from_ymd_opt(2023, 1, 4).unwrap()));

    // Check that the existing data is still there (by date, not price)
    assert!(aapl_prices.iter().any(|p| p.date == NaiveDate::from_ymd_opt(2023, 1, 3).unwrap()));
    assert!(aapl_prices.iter().any(|p| p.date == NaiveDate::from_ymd_opt(2023, 1, 5).unwrap()));

    // Check that data after the gap is also fetched
    assert!(aapl_prices.iter().any(|p| p.date == NaiveDate::from_ymd_opt(2023, 1, 6).unwrap()));

    Ok(())
}
