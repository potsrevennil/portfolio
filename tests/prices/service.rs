use chrono::NaiveDate;
use portfolio::prices::{PriceService, StockPriceStore};
use tokio;

#[path = "../common/mod.rs"]
mod common;

use common::create_temp_db;

#[tokio::test]
async fn test_get_price_on_date() -> anyhow::Result<()> {
    let (pool, _db_file) = create_temp_db().await?;
    let price_store = StockPriceStore::new(pool.clone());
    let price_service = PriceService::new(price_store);

    let symbol = "AAPL";
    let date = NaiveDate::from_ymd_opt(2023, 1, 3).unwrap();

    // 1. Test when price is not in DB, should fetch from web
    let price = price_service.get_price_on_date(symbol, date).await?;
    assert!(price.is_some());
    let price = price.unwrap();
    assert_eq!(price.date, date);

    // 2. Test when price is in DB
    let price2 = price_service.get_price_on_date(symbol, date).await?;
    assert!(price2.is_some());
    let price2 = price2.unwrap();
    assert_eq!(price2.date, date);
    assert_eq!(price.close_price, price2.close_price);

    Ok(())
}
