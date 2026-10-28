//! Builds a ledger from invented transactions: written as a journal and
//! loaded by the real load-journal, so labels come from the mapping. Not every
//! test binary uses every item.
#![allow(dead_code)]

use db::{assertions::BalanceAssertion, load, market::Market};
use ledger::{
    journal::{self, Journal, Posting},
    model::Source,
};
use ledger_types::currency::Currency;
use prices::YFinanceSource;
use rust_decimal::Decimal;
use sqlx::SqlitePool;
use tempfile::TempDir;

/// Legs of one transaction: (account, amount, currency).
pub type Legs<'a> = &'a [(&'a str, &'a str, Currency)];

/// One manual transaction per `(date, legs)`.
pub fn journal(txns: &[(&str, Legs)]) -> Journal {
    let postings = txns
        .iter()
        .enumerate()
        .flat_map(|(group, (date, legs))| {
            legs.iter().map(move |(account, amount, currency)| Posting {
                group: group as u64,
                source: Source::Manual,
                date: date.parse().unwrap(),
                payee: None,
                narration: "test".into(),
                external_ref: None,
                account: account.to_string(),
                amount: amount.parse::<Decimal>().unwrap(),
                currency: *currency,
                tags: None,
            })
        })
        .collect();
    Journal { postings }
}

/// The journal and the figures the load's gate checks it against, loaded
/// into a fresh database with an empty market cache beside it.
pub async fn load(
    mapping: &str,
    journal: &Journal,
    assertions: &[BalanceAssertion],
) -> (TempDir, SqlitePool, Market) {
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
        market_url: format!("sqlite:{}", root.join("market.db").display()),
        mapping: root.join("mapping.toml"),
    })
    .await
    .unwrap();
    let pool = db::init_db(&url).await.unwrap();
    let market = super::market::beside(root).await;
    (dir, pool, market)
}

/// One USD→TWD quote, the only rate the fixtures record.
pub async fn usd_rate(market: &Market, date: &str, close: f64) {
    super::market::quote(
        market,
        &YFinanceSource::ticker(Currency::USD, Currency::TWD),
        date,
        close,
    )
    .await;
}
