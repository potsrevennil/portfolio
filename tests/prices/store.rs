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

    let symbol = "TEST";
    let prices = vec![
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 1).unwrap(), close_price: 100.0 },
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 2).unwrap(), close_price: 101.0 },
        StockPrice { date: NaiveDate::from_ymd_opt(2023, 1, 3).unwrap(), close_price: 102.0 },
    ];

    price_store.save_stock_prices(symbol, &prices).await?;

    // Test get_latest_date
    let latest_date = price_store.get_latest_date(symbol).await?;
    assert_eq!(latest_date, Some(NaiveDate::from_ymd_opt(2023, 1, 3).unwrap()));

    // Test get_stock_prices_in_range
    let fetched_prices = price_store
        .get_stock_prices_in_range(
            symbol,
            NaiveDate::from_ymd_opt(2023, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2023, 1, 2).unwrap(),
        )
        .await?;

    assert_eq!(fetched_prices.len(), 2);
    assert_eq!(fetched_prices[0].date, NaiveDate::from_ymd_opt(2023, 1, 1).unwrap());
    assert_eq!(fetched_prices[0].close_price, 100.0);
    assert_eq!(fetched_prices[1].date, NaiveDate::from_ymd_opt(2023, 1, 2).unwrap());
    assert_eq!(fetched_prices[1].close_price, 101.0);

    Ok(())
}
