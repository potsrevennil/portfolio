#[path = "common/mod.rs"]
mod common;
#[path = "common/market.rs"]
mod market;
use common::create_temp_db;

/// Market data lives only in market.db; the ledger holds none of it.
#[tokio::test]
async fn quotes_and_splits_live_in_the_market_cache_not_the_ledger() -> sqlx::Result<()> {
    let (ledger, db_file) = create_temp_db().await.expect("Failed to create temp db");
    assert!(db_file.path().exists());
    let (market, _dir) = market::temp().await;

    let tables = |pool| async move {
        sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('stock_prices', \
             'stock_splits', 'price_checks') ORDER BY name",
        )
        .fetch_all(pool)
        .await
    };
    assert_eq!(tables(&ledger).await?, Vec::<String>::new());
    assert_eq!(tables(market.pool()).await?, ["price_checks", "stock_prices", "stock_splits"]);
    Ok(())
}

/// A typo in --database-url must fail, not create an empty ledger that every
/// check then passes.
#[tokio::test]
async fn open_db_refuses_a_database_that_does_not_exist() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("typo.db");
    let url = format!("sqlite:{}", missing.display());
    let err = db::open_db(&url).await.expect_err("a missing database must not be created");
    assert!(format!("{err:#}").contains("no database at"), "{err:#}");
    assert!(!missing.exists(), "the file was created anyway");
}
