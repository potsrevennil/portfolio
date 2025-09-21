use std::{env, fs};

use anyhow::Result;
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::main]
async fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=migrations");

    let db_path = "sqlite.db";
    let db_url = format!("sqlite:{}", db_path);

    // Create the database file if it doesn't exist
    if !fs::metadata(db_path).is_ok() {
        println!("Build script: Creating database file: {}", db_path);
        fs::File::create(db_path)?;
    }

    println!("Build script: Using database: {}", db_url);

    // Set DATABASE_URL for sqlx macros
    env::set_var("DATABASE_URL", &db_url);

    // Run migrations
    println!("Build script: Running migrations...");
    let pool = SqlitePoolOptions::new().connect(&db_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    println!("Build script: Migrations completed.");

    Ok(())
}
