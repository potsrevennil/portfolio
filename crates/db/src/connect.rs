use std::{fs, path::Path, str::FromStr};

use anyhow::{Context, Result};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};

/// Opens a database as it is: never creates one, never migrates it. A
/// mistyped path fails instead of serving an empty ledger.
pub async fn connect(url: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(url)?.create_if_missing(false);
    SqlitePool::connect_with(options).await.with_context(|| format!("opening {url}"))
}

/// Opens a database that must already exist: a read-only command pointed at a
/// typo would otherwise create an empty one and report it as healthy.
pub async fn open_db(db_url: &str) -> Result<SqlitePool> {
    let path = db_url.trim_start_matches("sqlite:");
    if Path::new(path).exists() {
        init_db(db_url).await
    } else {
        anyhow::bail!("no database at {path}")
    }
}

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
