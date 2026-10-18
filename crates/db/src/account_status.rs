//! Closing and reopening accounts: `accounts.closed`, with the dated
//! `account_events` row each change leaves. A closed asset or liability holds
//! nothing: it leaves every picker, so money in it would still count in the
//! totals with no way to post it out. An income or expense account sums what
//! flowed through it, so closing one only retires it.

use std::{collections::BTreeMap, fmt};

use anyhow::{bail, Context, Result};
use ledger::accounts::in_subtree;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{query::AccountType, sums::Sums};

const ACCOUNT_COLUMNS: &str =
    "SELECT id, path, type AS acct_type, closed != 0 AS closed FROM accounts";

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    path: String,
    acct_type: String,
    closed: bool,
}

pub(crate) struct Account {
    pub id: i64,
    pub path: String,
    pub account_type: AccountType,
    pub closed: bool,
}

impl TryFrom<Row> for Account {
    type Error = anyhow::Error;

    fn try_from(r: Row) -> Result<Self> {
        Ok(Account {
            account_type: r
                .acct_type
                .parse()
                .with_context(|| format!("account type {:?}", r.acct_type))?,
            id: r.id,
            path: r.path,
            closed: r.closed,
        })
    }
}

pub(crate) async fn find(db: &mut SqliteConnection, path: &str) -> Result<Account> {
    let row: Row = sqlx::query_as(&format!("{ACCOUNT_COLUMNS} WHERE path = ?"))
        .bind(path)
        .fetch_optional(db)
        .await?
        .with_context(|| format!("查無帳戶：{path}"))?;
    row.try_into()
}

async fn set(db: &mut SqliteConnection, id: i64, closed: bool, note: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE accounts SET closed = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE \
         id = ?",
    )
    .bind(closed)
    .bind(id)
    .execute(&mut *db)
    .await?;
    sqlx::query("INSERT INTO account_events (account_id, event, note) VALUES (?, ?, ?)")
        .bind(id)
        .bind(if closed { "closed" } else { "reopened" })
        .bind(note.map(str::trim).filter(|n| !n.is_empty()))
        .execute(&mut *db)
        .await?;
    Ok(())
}

/// One posting's amount, parsed in Rust: amounts are exact decimal strings,
/// which SQL `SUM` would turn into REAL.
fn leg(amount: &str, currency: &str) -> Result<(Currency, Decimal)> {
    Ok((
        currency.parse().with_context(|| format!("currency {currency:?}"))?,
        amount.parse().with_context(|| format!("amount {amount:?}"))?,
    ))
}

/// What each asset and liability account's own postings hold, by path; with
/// `only_closed`, just the closed ones'.
async fn held_by_account(
    db: &mut SqliteConnection,
    only_closed: bool,
) -> Result<BTreeMap<String, Sums>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT a.path, p.amount, p.currency FROM postings p JOIN accounts a ON a.id = \
         p.account_id WHERE a.type IN ('asset', 'liability') AND (?1 = 0 OR a.closed != 0)",
    )
    .bind(only_closed)
    .fetch_all(&mut *db)
    .await?;
    let mut legs: BTreeMap<String, Vec<(Currency, Decimal)>> = BTreeMap::new();
    for (path, amount, currency) in rows {
        legs.entry(path).or_default().push(leg(&amount, &currency)?);
    }
    let held = legs.into_iter().map(|(path, legs)| (path, legs.into_iter().collect::<Sums>()));
    Ok(held.filter(|(_, h)| !h.is_empty()).collect())
}

/// The closed assets and liabilities that hold money, by path.
pub async fn closed_holding(db: &mut SqliteConnection) -> Result<BTreeMap<String, Sums>> {
    held_by_account(db, true).await
}

/// Why an account can't be closed.
#[derive(Debug, PartialEq)]
pub enum Blocker {
    Closed,
    /// A root, or equity, which holds the loader's system accounts.
    Fixed,
    OpenUnder(usize),
    Holds(Sums),
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocker::Closed => f.write_str("帳戶已經結清"),
            Blocker::Fixed => f.write_str("這個帳戶不能結清"),
            Blocker::OpenUnder(n) => write!(f, "底下還有 {n} 個帳戶沒結清"),
            Blocker::Holds(held) => write!(f, "餘額不是 0，不能結清：{held}"),
        }
    }
}

/// What keeps each account that can't be closed open, by path; an account
/// missing here can be closed.
pub async fn blockers(db: &mut SqliteConnection) -> Result<BTreeMap<String, Blocker>> {
    let rows: Vec<Row> =
        sqlx::query_as(&format!("{ACCOUNT_COLUMNS} ORDER BY path")).fetch_all(&mut *db).await?;
    let accounts: Vec<Account> = rows.into_iter().map(Account::try_from).collect::<Result<_>>()?;
    let held = held_by_account(db, false).await?;
    let mut blockers = BTreeMap::new();
    for a in &accounts {
        let under = || accounts.iter().filter(|b| b.path != a.path && in_subtree(&b.path, &a.path));
        let open_under = under().filter(|b| !b.closed).count();
        let blocker = if a.closed {
            Some(Blocker::Closed)
        } else if a.account_type == AccountType::Equity || !a.path.contains(':') {
            Some(Blocker::Fixed)
        } else if open_under > 0 {
            Some(Blocker::OpenUnder(open_under))
        } else {
            let subtree = held.iter().filter(|(p, _)| in_subtree(p, &a.path));
            let holds: Sums = subtree.flat_map(|(_, h)| h.iter()).collect();
            (!holds.is_empty()).then_some(Blocker::Holds(holds))
        };
        if let Some(blocker) = blocker {
            blockers.insert(a.path.clone(), blocker);
        }
    }
    Ok(blockers)
}

/// Closes an account, unless [`blockers`] names it.
pub async fn close(pool: &SqlitePool, path: &str, note: Option<&str>) -> Result<()> {
    let mut db = pool.begin().await?;
    let row = find(&mut db, path).await?;
    if let Some(blocker) = blockers(&mut db).await?.remove(path) {
        bail!("{blocker}：{path}");
    }
    set(&mut db, row.id, true, note).await?;
    db.commit().await?;
    Ok(())
}

/// Reopens an account, and every closed account above it so it shows again.
pub async fn reopen(pool: &SqlitePool, path: &str, note: Option<&str>) -> Result<()> {
    let mut db = pool.begin().await?;
    let row = find(&mut db, path).await?;
    if !row.closed {
        bail!("帳戶沒有結清：{path}");
    }
    reopen_in(&mut db, path, note).await?;
    db.commit().await?;
    Ok(())
}

/// [`reopen`] inside the caller's transaction; an open account is left as it
/// is.
pub async fn reopen_in(db: &mut SqliteConnection, path: &str, note: Option<&str>) -> Result<()> {
    let ancestors = path.match_indices(':').map(|(i, _)| &path[..i]);
    for p in ancestors.chain([path]) {
        let row = find(db, p).await?;
        if row.closed {
            set(db, row.id, false, note).await?;
        }
    }
    Ok(())
}
