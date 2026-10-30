//! The user's own records, for acceptance tests on them. They are gitignored
//! and live in the main checkout (or `$LEDGER_RECORDS_DIR`), so tests skip
//! where they are absent, as on CI, and read them only.

use std::path::{Path, PathBuf};

use anyhow::Result;
use db::load;
use ledger::{args::Args as BuildArgs, freeze};
use sqlx::SqlitePool;
use tempfile::TempDir;

/// The records holding `raw/<party>`, if this machine has them.
pub fn dir(party: &str) -> Option<PathBuf> {
    let dir = match std::env::var_os("LEDGER_RECORDS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            // A worktree's records are its main checkout's.
            let out = std::process::Command::new("git")
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .ok()?;
            PathBuf::from(String::from_utf8(out.stdout).ok()?.trim()).parent()?.to_path_buf()
        }
    };
    dir.join("raw").join(party).is_dir().then_some(dir)
}

/// Every file under `raw/<party>/<folder>/` with extension `ext`.
pub fn files(records: &Path, party: &str, ext: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for folder in std::fs::read_dir(records.join("raw").join(party))? {
        let folder = folder?.path();
        if !folder.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(folder)? {
            let path = file?.path();
            if path.extension().is_some_and(|e| e == ext) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// A scratch ledger loaded from a fresh freeze of these statements. Keep the
/// directory for as long as the pool.
pub async fn frozen(
    records: &Path,
    cathay_statements: Vec<PathBuf>,
    line_bank_statements: Vec<PathBuf>,
) -> Result<(SqlitePool, TempDir)> {
    let scratch = TempDir::new()?;
    let journal = scratch.path().join("journal.csv");
    let frozen = freeze::run(&freeze::FreezeArgs {
        journal: journal.clone(),
        build: BuildArgs {
            cathay_statements,
            line_bank_statements,
            daily_income_expense: None,
            daily_transfers: None,
            transactions: Some(records.join("corrected/transactions.csv")),
            backfill: false,
            ledger_dir: records.join("ledger"),
        },
    })?;
    anyhow::ensure!(frozen.ok(), "freeze failed:\n{frozen}");
    let url = format!("sqlite:{}", scratch.path().join("frozen.db").display());
    load::run(&load::Args {
        journal,
        database_url: url.clone(),
        market_url: format!("{url}.market"),
        mapping: records.join("ledger/mapping.toml"),
    })
    .await?;
    Ok((db::init_db(&url).await?, scratch))
}
