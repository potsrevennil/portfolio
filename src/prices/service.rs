use anyhow::Result;
use chrono::NaiveDate;

use super::{
    source::{PriceError, StockPrice, YFinanceSource},
    store::StockPriceStore,
};

pub struct PriceService {
    store: StockPriceStore,
}

impl PriceService {
    pub fn new(store: StockPriceStore) -> Self { Self { store } }

    pub async fn get_price_on_date(
        &self,
        symbol: &str,
        date: NaiveDate,
    ) -> Result<Option<StockPrice>, PriceError> {
        // 1. Try to get from local store
        let prices_in_range = self.store.get_stock_prices_in_range(symbol, date, date).await?;
        if let Some(price) = prices_in_range.into_iter().next() {
            return Ok(Some(price));
        }

        // 2. If not in store, fetch from web
        let fetched_prices = YFinanceSource::fetch_stock_prices(symbol, date, date).await?;

        // 3. Save to store
        if !fetched_prices.is_empty() {
            self.store.save_stock_prices(symbol, &fetched_prices).await?;
        }

        // 4. Return the price
        Ok(fetched_prices.into_iter().next())
    }
}
