//! Creates and migrates the database this crate's sqlx query macros check
//! against. It lives at the workspace root: in a workspace the macros resolve
//! a relative `DATABASE_URL` from there, while this script runs in `crates/db`.
//!
//! The migrations are read when the script runs, not embedded with
//! `sqlx::migrate!`: cargo recompiles a build script only when a file it
//! already read changes, so a newly added migration would rerun a stale binary
//! that lacks it, and later fail with "previously applied but is missing".

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use sqlx::{migrate::Migrator, sqlite::SqlitePoolOptions};

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
    Migrator::new(manifest_dir.join("migrations")).await?.run(&pool).await?;
    Ok(())
}
