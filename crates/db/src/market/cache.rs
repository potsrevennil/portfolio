//! Opening the cache, and the settings every caller shares.

use std::str::FromStr;

use anyhow::{Context, Result};
use ledger_types::currency::Currency;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};

use crate::quotes::StockPriceStore;

pub const DEFAULT_URL: &str = "sqlite:market.db";

/// The currency every page and report converts into.
pub const BASE: Currency = Currency::TWD;

/// The market cache. Its own type, so a page's context never confuses it with
/// the ledger pool.
#[derive(Clone, Debug)]
pub struct Market(SqlitePool);

impl Market {
    /// Opens the cache for writing, creating and migrating it as needed: it
    /// holds nothing that cannot be fetched again.
    pub async fn open(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .with_context(|| format!("opening {url}"))?;
        sqlx::migrate!("./market-migrations")
            .run(&pool)
            .await
            .with_context(|| format!("migrating {url}"))?;
        Ok(Self(pool))
    }

    /// Opens an existing cache read-only, for reporting.
    pub async fn read_only(url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(url)?.read_only(true);
        let pool =
            SqlitePool::connect_with(options).await.with_context(|| format!("opening {url}"))?;
        Ok(Self(pool))
    }

    pub fn pool(&self) -> &SqlitePool { &self.0 }

    pub fn prices(&self) -> StockPriceStore { StockPriceStore::new(self.0.clone()) }
}
