use std::{fs, path::Path};

use anyhow::{Context, Result};
use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

pub async fn init_db(db_url: &str) -> Result<SqlitePool> {
    let db_file_path = db_url.trim_start_matches("sqlite:");

    // Create the database file if it doesn't exist
    if !Path::new(db_file_path).exists() {
        log::info!("Creating database file: {db_file_path}");
        fs::File::create(db_file_path)?;
    }

    let pool = SqlitePoolOptions::new().max_connections(5).connect(db_url).await?;

    // Run migrations using sqlx::migrate! macro
    sqlx::migrate!("./migrations").run(&pool).await.context("Failed to run migrations")?;

    Ok(pool)
}
