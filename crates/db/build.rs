//! Creates and migrates the database this crate's sqlx query macros check
//! against: `sqlx-check.db` at the workspace root, where the macros resolve a
//! relative `DATABASE_URL`, while this script runs in `crates/db`. It is
//! rebuilt from empty on every run, so a migration edited in place never trips
//! sqlx's checksum check. Nothing else lives there, so no build touches a real
//! file.
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
    let db_path = manifest_dir.join("../../sqlx-check.db");
    for side in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{side}", db_path.display()));
        if path.exists() {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
    }
    fs::File::create(&db_path).with_context(|| format!("creating {}", db_path.display()))?;
    let db_url = format!("sqlite:{}", db_path.display());
    let pool = SqlitePoolOptions::new().connect(&db_url).await?;
    Migrator::new(manifest_dir.join("migrations")).await?.run(&pool).await?;
    Ok(())
}
