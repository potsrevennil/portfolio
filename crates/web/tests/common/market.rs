//! The market cache beside a test's ledger.

#![allow(dead_code)]

use std::{collections::HashMap, path::Path};

use db::market::Market;
use prices::{PriceStore, StockPrice};

/// The cache in `dir`, created empty on first use.
pub async fn beside(dir: &Path) -> Market {
    Market::open(&format!("sqlite:{}", dir.join("market.db").display())).await.unwrap()
}

/// One close for `ticker`, through the store the fetch writes with.
pub async fn quote(market: &Market, ticker: &str, date: &str, close_price: f64) {
    let quote = StockPrice { date: date.parse().unwrap(), close_price };
    market
        .prices()
        .save_stock_prices(&HashMap::from([(ticker.into(), vec![quote])]))
        .await
        .unwrap();
}
