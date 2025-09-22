use sqlx::Row;
use tokio;

#[path = "common/mod.rs"]
mod common;
use common::create_temp_db;

#[tokio::test]
async fn test_init_db_creates_schema() -> sqlx::Result<()> {
    let (pool, db_file) = create_temp_db().await.expect("Failed to create temp db");

    // Verify that the database file exists
    assert!(db_file.path().exists());

    // Verify that the stock_prices table exists by querying sqlite_master
    let row =
        sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name='stock_prices';")
            .fetch_one(&pool)
            .await?;

    assert_eq!(row.get::<&str, _>(0), "stock_prices");

    Ok(())
}
