//! The **journal loader**: the app's only historical-load path.
//!
//! It reads a trusted [`journal`](super::journal) — already reconciled and
//! verified by the [freeze tool](super::freeze) — and writes it into the SQLite
//! core schema. Deliberately dumb: no reconciliation, no chart, no Beancount.
//! It derives the chart of accounts from the paths the journal mentions (type
//! from the root, label from the leaf) and, as a cheap tripwire, refuses a
//! transaction whose legs do not sum to zero per currency. It is one-time: it
//! refuses a non-empty database.
//!
//! ```text
//! cargo run -- journal-load --journal ledger/journal.csv --database-url sqlite:ledger-app.db
//! ```

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{Row, SqlitePool};

use super::{accounts::AccountType, journal};
use crate::{currency::Currency, db};

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// The reconciled journal CSV to load (produced by `freeze`).
    #[arg(long, default_value = "ledger/journal.csv")]
    pub journal: PathBuf,

    /// SQLite database to load into. Must be empty of transactions.
    #[arg(long, default_value = "sqlite:ledger-app.db")]
    pub database_url: String,
}

/// The `accounts.type` value for an account path's root.
fn schema_type(path: &str) -> Result<&'static str> {
    let root: AccountType = path.split(':').next().unwrap_or("").parse().map_err(|_| {
        anyhow::anyhow!("{path:?} is not a Beancount account (no Assets/Liabilities/… root)")
    })?;
    Ok(match root {
        AccountType::Assets => "asset",
        AccountType::Liabilities => "liability",
        AccountType::Equity => "equity",
        AccountType::Income => "income",
        AccountType::Expenses => "expense",
    })
}

/// What the loader wrote.
#[derive(Debug)]
pub struct Report {
    pub accounts: usize,
    pub transactions: usize,
    pub postings: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "loaded journal into SQLite:")?;
        writeln!(f, "  {} accounts", self.accounts)?;
        writeln!(f, "  {} transactions, {} postings", self.transactions, self.postings)?;
        Ok(())
    }
}

pub async fn run(args: &Args) -> Result<Report> {
    let journal = journal::read(&args.journal)?;
    let pool = db::init_db(&args.database_url).await?;
    guard_empty(&pool).await?;

    let mut tx = pool.begin().await?;

    // Accounts are exactly the paths the journal mentions. An account is a
    // placeholder if any of its legs carries the reserved tag.
    let placeholder_accounts: BTreeSet<&str> = journal
        .postings
        .iter()
        .filter(|p| p.tags.as_deref().is_some_and(|t| t.contains(journal::PLACEHOLDER_TAG)))
        .map(|p| p.account.as_str())
        .collect();
    let paths: BTreeSet<&str> = journal.postings.iter().map(|p| p.account.as_str()).collect();

    let mut ids: BTreeMap<String, i64> = BTreeMap::new();
    for path in paths {
        let label = path.rsplit(':').next().unwrap_or(path);
        let id =
            sqlx::query("INSERT INTO accounts (path, label, type, closed) VALUES (?, ?, ?, 0)")
                .bind(path)
                .bind(label)
                .bind(schema_type(path)?)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("inserting account {path}"))?
                .last_insert_rowid();
        ids.insert(path.to_string(), id);

        let note = placeholder_accounts.contains(path).then_some(
            "securities value backfilled by the freeze; replace with real positions later (do not \
             double-count)",
        );
        sqlx::query(
            "INSERT INTO account_events (account_id, event, note) VALUES (?, 'created', ?)",
        )
        .bind(id)
        .bind(note)
        .execute(&mut *tx)
        .await?;
    }

    let mut posting_count = 0;
    // Regroup the legs into transactions by group id.
    let mut groups: BTreeMap<u64, Vec<&journal::Posting>> = BTreeMap::new();
    for p in &journal.postings {
        groups.entry(p.group).or_default().push(p);
    }

    for (group, legs) in &groups {
        // Tripwire: a transaction whose legs do not sum to zero per currency is
        // a corrupt journal, and loading it would break the double-entry invariant.
        let mut residual: BTreeMap<Currency, Decimal> = BTreeMap::new();
        for leg in legs {
            *residual.entry(leg.currency).or_default() += leg.amount;
        }
        if let Some((currency, amount)) = residual.iter().find(|(_, a)| !a.is_zero()) {
            bail!(
                "journal transaction group {group} does not balance: {amount} {currency} left over"
            );
        }

        let header = legs[0];
        let txn_id = insert_transaction(
            &mut tx,
            header.date,
            header.payee.as_deref(),
            Some(&header.narration),
            &header.source,
            header.external_ref.as_deref(),
        )
        .await
        .context("inserting transaction")?;

        for leg in legs {
            let account_id =
                ids.get(&leg.account).context("posting to an account with no chart row")?;
            insert_posting(
                &mut tx,
                txn_id,
                *account_id,
                leg.amount,
                leg.currency,
                leg.tags.as_deref(),
            )
            .await?;
            posting_count += 1;
        }
    }

    tx.commit().await?;
    log::info!("journal-load: committed");
    Ok(Report { accounts: ids.len(), transactions: groups.len(), postings: posting_count })
}

async fn insert_transaction(
    tx: &mut sqlx::SqliteConnection,
    date: NaiveDate,
    payee: Option<&str>,
    narration: Option<&str>,
    source: &str,
    external_ref: Option<&str>,
) -> Result<i64> {
    Ok(sqlx::query(
        "INSERT INTO transactions (date, payee, narration, source, external_ref, reviewed) VALUES \
         (?, ?, ?, ?, ?, 0)",
    )
    .bind(date.to_string())
    .bind(payee)
    .bind(narration)
    .bind(source)
    .bind(external_ref)
    .execute(tx)
    .await?
    .last_insert_rowid())
}

async fn insert_posting(
    tx: &mut sqlx::SqliteConnection,
    transaction_id: i64,
    account_id: i64,
    amount: Decimal,
    currency: Currency,
    tags: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO postings (transaction_id, account_id, amount, currency, tags) VALUES (?, ?, \
         ?, ?, ?)",
    )
    .bind(transaction_id)
    .bind(account_id)
    .bind(amount.to_string())
    .bind(currency.to_string())
    .bind(tags)
    .execute(tx)
    .await
    .context("inserting posting")?;
    Ok(())
}

/// Refuses a database that already holds history — the load is one-time.
async fn guard_empty(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM transactions").fetch_one(pool).await?.get(0);
    if count > 0 {
        bail!(
            "database already contains {count} transactions; the journal load is one-time — point \
             --database-url at a fresh file"
        );
    }
    Ok(())
}
