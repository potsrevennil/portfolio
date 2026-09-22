//! Creates and migrates the database this crate's sqlx query macros check
//! against. It lives at the workspace root: in a workspace the macros resolve
//! a relative `DATABASE_URL` from there, while this script runs in `crates/db`.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use sqlx::sqlite::SqlitePoolOptions;

#[tokio::main]
async fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=migrations");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let db_path = manifest_dir.join("../../sqlite.db");
    if !db_path.exists() {
        fs::File::create(&db_path).with_context(|| format!("creating {}", db_path.display()))?;
    }
    let db_url = format!("sqlite:{}", db_path.display());
    let pool = SqlitePoolOptions::new().connect(&db_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(())
}
