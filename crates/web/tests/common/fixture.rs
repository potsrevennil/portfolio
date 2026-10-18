//! A synthetic ledger as the app holds it: a journal and its assertions
//! loaded by the real load-journal, labelled by a mapping.
#![allow(dead_code)]

use db::{assertions::BalanceAssertion, load};
use ledger::{
    journal::{self, Journal, Posting},
    model::Source,
};
use ledger_types::currency::Currency;
use sqlx::SqlitePool;
use tempfile::TempDir;

/// One leg of transaction `group`, entered by hand; callers override the
/// rest with struct update syntax.
pub fn posting(
    group: usize,
    date: &str,
    account: &str,
    amount: &str,
    currency: Currency,
) -> Posting {
    Posting {
        group: group as u64,
        source: Source::Manual,
        date: date.parse().unwrap(),
        payee: None,
        narration: "test".into(),
        external_ref: None,
        account: account.into(),
        amount: amount.parse().unwrap(),
        currency,
        tags: None,
    }
}

/// `mapping.toml`, the journal and its assertions written to a fresh
/// directory and loaded into `ledger-app.db` there.
pub async fn load(
    mapping: &str,
    journal: &Journal,
    assertions: &[BalanceAssertion],
) -> (TempDir, SqlitePool) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(root.join("mapping.toml"), mapping).unwrap();
    let journal_path = root.join("journal.csv");
    journal::write(&journal_path, journal).unwrap();
    journal::write_assertions(&journal::assertions_path(&journal_path), assertions).unwrap();
    let url = format!("sqlite:{}", root.join("ledger-app.db").display());
    load::run(&load::Args {
        journal: journal_path,
        database_url: url.clone(),
        mapping: root.join("mapping.toml"),
    })
    .await
    .unwrap();
    (dir, db::init_db(&url).await.unwrap())
}
