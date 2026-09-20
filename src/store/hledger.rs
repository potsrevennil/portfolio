//! Exports the SQLite ledger as an hledger journal, for `hledger check` as an
//! independent audit. Balance assertions become zero-amount postings with a
//! subtree assertion (`=*`) at the end of their day.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use sqlx::SqliteConnection;

use super::assertions;

#[derive(sqlx::FromRow)]
struct PostingRow {
    transaction_id: i64,
    date: String,
    payee: Option<String>,
    narration: Option<String>,
    path: String,
    amount: String,
    currency: String,
    tags: Option<String>,
}

/// One dated block of the journal. Sorting puts a day's assertions after its
/// transactions, since each assertion describes the end of its day.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Entry {
    Transaction(i64),
    Assertion(usize),
}

/// The whole ledger as hledger journal text.
pub async fn export(conn: &mut SqliteConnection) -> Result<String> {
    let rows: Vec<PostingRow> = sqlx::query_as(
        "SELECT t.id AS transaction_id, t.date, t.payee, t.narration, a.path, p.amount, \
         p.currency, p.tags FROM postings p JOIN transactions t ON t.id = p.transaction_id JOIN \
         accounts a ON a.id = p.account_id ORDER BY t.id, p.id",
    )
    .fetch_all(&mut *conn)
    .await
    .context("loading postings for the hledger export")?;
    let assertions = assertions::load(conn).await?;
    let accounts: Vec<String> =
        sqlx::query_scalar("SELECT path FROM accounts ORDER BY path").fetch_all(&mut *conn).await?;

    let mut transactions: BTreeMap<i64, Vec<&PostingRow>> = BTreeMap::new();
    let mut entries: BTreeSet<(NaiveDate, Entry)> = BTreeSet::new();
    for r in &rows {
        let date = r.date.parse().with_context(|| format!("transaction date {:?}", r.date))?;
        entries.insert((date, Entry::Transaction(r.transaction_id)));
        transactions.entry(r.transaction_id).or_default().push(r);
    }
    for (i, a) in assertions.iter().enumerate() {
        for (date, _) in a.points() {
            entries.insert((date, Entry::Assertion(i)));
        }
    }

    let mut out = String::from("decimal-mark .\n\n");
    // Assertions can name a currency no posting mentions, and --strict rejects
    // an undeclared commodity.
    let commodities: BTreeSet<String> = rows
        .iter()
        .map(|r| r.currency.clone())
        .chain(assertions.iter().map(|a| a.currency.to_string()))
        .collect();
    for c in commodities {
        writeln!(out, "commodity {c}")?;
    }
    for a in &accounts {
        writeln!(out, "account {a}")?;
    }

    for (date, entry) in entries {
        out.push('\n');
        match entry {
            Entry::Transaction(id) => {
                let legs = &transactions[&id];
                let first = legs[0];
                let description = [first.payee.as_deref(), first.narration.as_deref()]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .map(clean)
                    .collect::<Vec<_>>()
                    .join(" | ");
                writeln!(out, "{date} {description}")?;
                for leg in legs {
                    write!(out, "    {}  {} {}", leg.path, leg.amount, leg.currency)?;
                    match &leg.tags {
                        Some(tags) => writeln!(out, "  ; tags: {}", clean(tags))?,
                        None => out.push('\n'),
                    }
                }
            }
            Entry::Assertion(i) => {
                let a = &assertions[i];
                let expected = a.points().find(|(d, _)| *d == date).map(|(_, e)| e);
                let expected = expected.context("assertion point vanished")?;
                writeln!(out, "{date} {} balance", a.source)?;
                writeln!(out, "    {}  0 {c} =* {expected} {c}", a.account, c = a.currency)?;
            }
        }
    }
    Ok(out)
}

/// hledger reads `;` as a comment and a newline as the end of the entry.
fn clean(s: &str) -> String { s.replace([';', '\n', '\r'], " ") }
