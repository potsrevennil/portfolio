//! The **seed loader** (design doc T2): the app's only historical-load path.
//!
//! It reads a trusted [`seed`](super::seed) — already reconciled and verified
//! by the [freeze tool](super::freeze) — and writes it into the SQLite core
//! schema. Deliberately dumb: no reconciliation, no chart, no Beancount. It
//! derives the chart of accounts from the paths the seed mentions (type from
//! the root, label from the leaf) and, as a cheap tripwire, refuses a
//! transaction whose legs do not sum to zero per currency. It is one-time: it
//! refuses a non-empty database.
//!
//! ```text
//! cargo run -- seed-load --seed ledger/seed.csv --database-url sqlite:ledger-app.db
//! ```

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
};

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use sqlx::{Row, SqlitePool};

use super::{accounts::AccountType, seed};
use crate::{currency::Currency, db};

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// The reconciled seed CSV to load (produced by `freeze`).
    #[arg(long, default_value = "ledger/seed.csv")]
    pub seed: PathBuf,

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
    pub openings: usize,
    pub transactions: usize,
    pub postings: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "loaded seed into SQLite:")?;
        writeln!(f, "  {} accounts", self.accounts)?;
        writeln!(f, "  {} opening balances", self.openings)?;
        writeln!(f, "  {} transactions, {} postings", self.transactions, self.postings)?;
        Ok(())
    }
}

pub async fn run(args: &Args) -> Result<Report> {
    let seed = seed::read(&args.seed)?;
    let pool = db::init_db(&args.database_url).await?;
    guard_empty(&pool).await?;

    let mut tx = pool.begin().await?;

    // Accounts are exactly the paths the seed mentions. An account is a
    // placeholder if any of its legs carries the reserved tag.
    let placeholder_accounts: BTreeSet<&str> = seed
        .postings
        .iter()
        .filter(|p| p.tags.as_deref().is_some_and(|t| t.contains(seed::PLACEHOLDER_TAG)))
        .map(|p| p.account.as_str())
        .collect();
    let paths: BTreeSet<&str> = seed
        .postings
        .iter()
        .map(|p| p.account.as_str())
        .chain(seed.openings.iter().map(|o| o.account.as_str()))
        .collect();

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
            "securities value backfilled by the T2 freeze; retire and replace with real positions \
             in T11 (do not double-count)",
        );
        sqlx::query(
            "INSERT INTO account_events (account_id, event, note) VALUES (?, 'created', ?)",
        )
        .bind(id)
        .bind(note)
        .execute(&mut *tx)
        .await?;
    }

    for o in &seed.openings {
        let id = ids.get(&o.account).context("opening for an account with no chart row")?;
        sqlx::query(
            "INSERT INTO opening_balances (account_id, currency, amount, date) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(o.currency.to_string())
        .bind(o.amount.to_string())
        .bind(o.date.to_string())
        .execute(&mut *tx)
        .await
        .with_context(|| format!("inserting opening balance for {}", o.account))?;
    }

    // Group the postings back into transactions by their group id, preserving
    // order.
    let mut groups: BTreeMap<u64, Vec<&seed::Posting>> = BTreeMap::new();
    for p in &seed.postings {
        groups.entry(p.group).or_default().push(p);
    }

    let mut posting_count = 0;
    for (group, legs) in &groups {
        // Tripwire: a transaction whose legs do not sum to zero per currency is
        // a corrupt seed, and loading it would break the double-entry invariant.
        let mut residual: BTreeMap<Currency, Decimal> = BTreeMap::new();
        for leg in legs {
            *residual.entry(leg.currency).or_default() += leg.amount;
        }
        if let Some((currency, amount)) = residual.iter().find(|(_, a)| !a.is_zero()) {
            bail!("seed transaction group {group} does not balance: {amount} {currency} left over");
        }

        let header = legs[0];
        let txn_id = sqlx::query(
            "INSERT INTO transactions (date, payee, narration, source, external_ref, reviewed) \
             VALUES (?, ?, ?, ?, ?, 0)",
        )
        .bind(header.date.to_string())
        .bind(&header.payee)
        .bind(&header.narration)
        .bind(&header.source)
        .bind(&header.external_ref)
        .execute(&mut *tx)
        .await
        .context("inserting transaction")?
        .last_insert_rowid();

        for leg in legs {
            let account_id =
                ids.get(&leg.account).context("posting to an account with no chart row")?;
            sqlx::query(
                "INSERT INTO postings (transaction_id, account_id, amount, currency, tags) VALUES \
                 (?, ?, ?, ?, ?)",
            )
            .bind(txn_id)
            .bind(account_id)
            .bind(leg.amount.to_string())
            .bind(leg.currency.to_string())
            .bind(&leg.tags)
            .execute(&mut *tx)
            .await
            .context("inserting posting")?;
            posting_count += 1;
        }
    }

    tx.commit().await?;
    log::info!("seed-load: committed");
    Ok(Report {
        accounts: ids.len(),
        openings: seed.openings.len(),
        transactions: groups.len(),
        postings: posting_count,
    })
}

/// Refuses a database that already holds history — the load is one-time.
async fn guard_empty(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM transactions").fetch_one(pool).await?.get(0);
    if count > 0 {
        bail!(
            "database already contains {count} transactions; the seed load is one-time — point \
             --database-url at a fresh file"
        );
    }
    Ok(())
}
