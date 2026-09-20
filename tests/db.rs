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

/// A typo in --database-url must fail, not create an empty ledger that every
/// check then passes.
#[tokio::test]
async fn open_db_refuses_a_database_that_does_not_exist() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("typo.db");
    let url = format!("sqlite:{}", missing.display());
    let err =
        portfolio::db::open_db(&url).await.expect_err("a missing database must not be created");
    assert!(format!("{err:#}").contains("no database at"), "{err:#}");
    assert!(!missing.exists(), "the file was created anyway");
}
