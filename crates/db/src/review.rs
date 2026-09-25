//! What the review queue writes: an edit or split of a transaction's legs in
//! place, a confirmation, and a hand-entered transaction. Each commits only
//! with its `transaction_events` row and only if it breaks no balance a
//! statement, 天天記帳 or a count vouches for. Refusals are worded for the page
//! that shows them.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger::{accounts::in_subtree, model::Source};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    assertions::AssertionSource,
    check::{self, Figure, Mismatch},
    events::{self, Change, EventKind},
    import::Origin,
    query::AccountType,
};

/// One leg as the editor submits it.
#[derive(Clone, Debug, PartialEq)]
pub struct LegEdit {
    /// The posting it rewrites; `None` for a leg the edit adds.
    pub posting_id: Option<i64>,
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
}

/// A transaction's legs and note as they should stand. Legs it leaves out
/// are removed; the rest keep their row, tags and, if unchanged, origin.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub narration: Option<String>,
    pub legs: Vec<LegEdit>,
    /// Mark it reviewed in the same write.
    pub confirm: bool,
}

/// A transaction entered by hand: `amount` leaves `account` for `counter`,
/// or for an expense `category`; an income category brings it in. A negative
/// amount runs the other way.
#[derive(Clone, Debug, PartialEq)]
pub struct Manual {
    pub date: NaiveDate,
    pub amount: Decimal,
    pub currency: Currency,
    pub account: String,
    pub category: Option<String>,
    pub counter: Option<String>,
    pub note: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PostingRow {
    id: i64,
    path: String,
    amount: String,
    currency: String,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: i64,
    acct_type: String,
    closed: bool,
}

struct Account {
    id: i64,
    account_type: AccountType,
    closed: bool,
}

async fn account(db: &mut SqliteConnection, path: &str) -> Result<Account> {
    let row: AccountRow = sqlx::query_as(
        "SELECT id, type AS acct_type, closed != 0 AS closed FROM accounts WHERE path = ?",
    )
    .bind(path)
    .fetch_optional(&mut *db)
    .await?
    .with_context(|| format!("查無帳戶：{path}"))?;
    Ok(Account {
        id: row.id,
        account_type: row
            .acct_type
            .parse()
            .with_context(|| format!("account type {:?}", row.acct_type))?,
        closed: row.closed,
    })
}

/// An account a new leg may post to.
async fn open_account(db: &mut SqliteConnection, path: &str) -> Result<Account> {
    let a = account(db, path).await?;
    if a.closed {
        bail!("帳戶已結清：{path}");
    }
    Ok(a)
}

fn balanced(legs: impl IntoIterator<Item = (Decimal, Currency)>) -> Result<()> {
    let mut sums: BTreeMap<Currency, Decimal> = BTreeMap::new();
    let mut any = false;
    for (amount, currency) in legs {
        if amount.is_zero() {
            bail!("金額不可為 0");
        }
        *sums.entry(currency).or_default() += amount;
        any = true;
    }
    if !any {
        bail!("交易沒有任何一筆");
    }
    let off: Vec<String> = sums
        .iter()
        .filter(|(_, sum)| !sum.is_zero())
        .map(|(c, sum)| format!("{sum} {c}"))
        .collect();
    match off.is_empty() {
        true => Ok(()),
        false => bail!("借貸不平衡，差 {}", off.join("、")),
    }
}

/// The figures the ledger disagrees with before a write: a write is held
/// only to the ones it breaks, not to one already broken elsewhere.
async fn baseline(db: &mut SqliteConnection) -> Result<Vec<Mismatch>> {
    Ok(check::check(db).await?.mismatches)
}

/// Commits `db` unless the write broke a balance an outside figure vouches
/// for; then it says which one, and what to do, in the reader's words.
async fn gated(mut db: sqlx::Transaction<'_, sqlx::Sqlite>, before: Vec<Mismatch>) -> Result<()> {
    let after = check::check(&mut db).await?;
    if let Some(broken) = after.mismatches.iter().find(|m| !before.contains(m)) {
        let refusal = refusal(&mut db, broken).await?;
        bail!(refusal);
    }
    db.commit().await?;
    Ok(())
}

async fn refusal(db: &mut SqliteConnection, m: &Mismatch) -> Result<String> {
    let account = match &m.figure {
        Figure::Balance(a) => &a.account,
        Figure::Holding(h) => &h.account,
    };
    let label: String = sqlx::query_scalar("SELECT label FROM accounts WHERE path = ?")
        .bind(account)
        .fetch_optional(db)
        .await?
        .unwrap_or_else(|| account.clone());
    let day = m.as_of;
    Ok(match &m.figure {
        Figure::Balance(a) => {
            let who = match a.source {
                AssertionSource::Statement => "對帳單",
                AssertionSource::Tiantian => "天天記帳",
                AssertionSource::Counted => "你盤點",
            };
            format!(
                "沒有儲存：這筆會讓「{label}」{day} 的餘額變成 {} {c}，但{who}記那天是 {} \
                 {c}。{day} 以前的帳已經對過了：這筆可能已經記過，或者日期該在 {day} 之後。",
                m.computed.normalize(),
                m.expected.normalize(),
                c = a.currency,
            )
        }
        Figure::Holding(_) => format!("沒有儲存：這筆會對不上「{label}」{day} 的持股。"),
    })
}

/// Rewrites a transaction's legs and note in place.
pub async fn edit(pool: &SqlitePool, id: i64, edit: &Edit) -> Result<()> {
    let mut db = pool.begin().await?;
    let before_check = baseline(&mut db).await?;
    let before = events::snapshot(&mut db, id).await?;
    let rows: Vec<PostingRow> = sqlx::query_as(
        "SELECT p.id, a.path, p.amount, p.currency FROM postings p
         JOIN accounts a ON a.id = p.account_id WHERE p.transaction_id = ? ORDER BY p.id",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    let held: HashMap<i64, &PostingRow> = rows.iter().map(|r| (r.id, r)).collect();

    balanced(edit.legs.iter().map(|l| (l.amount, l.currency)))?;
    let mut kept = HashSet::new();
    for leg in &edit.legs {
        if let Some(pid) = leg.posting_id {
            if !held.contains_key(&pid) || !kept.insert(pid) {
                bail!("第 {pid} 筆不屬於這筆交易");
            }
        }
    }

    for leg in &edit.legs {
        let unchanged = leg.posting_id.and_then(|pid| held.get(&pid)).is_some_and(|r| {
            r.path == leg.account
                && r.amount.parse::<Decimal>().ok() == Some(leg.amount)
                && r.currency == leg.currency.to_string()
        });
        if unchanged {
            continue;
        }
        let same_account =
            leg.posting_id.and_then(|pid| held.get(&pid)).is_some_and(|r| r.path == leg.account);
        // Changing an amount on an account keeps it; moving money onto one
        // needs it open.
        let target = match same_account {
            true => account(&mut db, &leg.account).await?,
            false => open_account(&mut db, &leg.account).await?,
        };
        match leg.posting_id {
            Some(pid) => {
                sqlx::query(
                    "UPDATE postings SET account_id = ?, amount = ?, currency = ?, origin = ? \
                     WHERE id = ?",
                )
                .bind(target.id)
                .bind(leg.amount.to_string())
                .bind(leg.currency.to_string())
                .bind(Origin::Manual.to_string())
                .bind(pid)
                .execute(&mut *db)
                .await?;
            }
            None => {
                sqlx::query(
                    "INSERT INTO postings (transaction_id, account_id, amount, currency, origin) \
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(id)
                .bind(target.id)
                .bind(leg.amount.to_string())
                .bind(leg.currency.to_string())
                .bind(Origin::Manual.to_string())
                .execute(&mut *db)
                .await?;
            }
        }
    }
    for r in rows.iter().filter(|r| !kept.contains(&r.id)) {
        sqlx::query("DELETE FROM postings WHERE id = ?").bind(r.id).execute(&mut *db).await?;
    }
    sqlx::query(
        "UPDATE transactions SET narration = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', \
         'now') WHERE id = ?",
    )
    .bind(edit.narration.as_deref().map(str::trim).filter(|n| !n.is_empty()))
    .bind(id)
    .execute(&mut *db)
    .await?;

    let after = events::snapshot(&mut db, id).await?;
    if after != before {
        let kind = match after.legs.len() > before.legs.len() {
            true => EventKind::Split,
            false => EventKind::Edited,
        };
        events::record(&mut db, id, kind, &Change { before: Some(before), after }).await?;
    }
    if edit.confirm {
        confirm_in(&mut db, id).await?;
    }
    gated(db, before_check).await
}

/// Marks a transaction reviewed as it stands. Confirming a reviewed one
/// changes nothing and records nothing.
pub async fn confirm(pool: &SqlitePool, id: i64) -> Result<()> {
    let mut db = pool.begin().await?;
    confirm_in(&mut db, id).await?;
    db.commit().await?;
    Ok(())
}

async fn confirm_in(db: &mut SqliteConnection, id: i64) -> Result<()> {
    let before = events::snapshot(db, id).await?;
    if before.reviewed {
        return Ok(());
    }
    sqlx::query(
        "UPDATE transactions SET reviewed = 1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *db)
    .await?;
    let after = events::snapshot(db, id).await?;
    events::record(db, id, EventKind::Confirmed, &Change { before: Some(before), after }).await
}

/// Books a hand-entered transaction, reviewed. Returns its id.
pub async fn enter(pool: &SqlitePool, m: &Manual) -> Result<i64> {
    let mut db = pool.begin().await?;
    let before = baseline(&mut db).await?;
    if m.amount.is_zero() {
        bail!("金額不可為 0");
    }
    let from = open_account(&mut db, &m.account).await?;
    if !matches!(from.account_type, AccountType::Asset | AccountType::Liability) {
        bail!("帳戶要是資產或負債：{}", m.account);
    }
    let (other, flow) = match (&m.category, &m.counter) {
        (Some(category), None) => {
            let a = open_account(&mut db, category).await?;
            let flow = match a.account_type {
                // Spent from the account; income comes into it.
                AccountType::Expense => -m.amount,
                AccountType::Income => m.amount,
                _ => bail!("類別要是收入或支出：{category}"),
            };
            (a, flow)
        }
        (None, Some(counter)) => {
            let a = open_account(&mut db, counter).await?;
            if !matches!(a.account_type, AccountType::Asset | AccountType::Liability) {
                bail!("對方帳戶要是資產或負債：{counter}");
            }
            if in_subtree(counter, &m.account) || in_subtree(&m.account, counter) {
                bail!("對方帳戶不可和帳戶相同");
            }
            (a, -m.amount)
        }
        _ => bail!("類別和對方帳戶要選一個，也只能選一個"),
    };

    let note = m.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let id = sqlx::query(
        "INSERT INTO transactions (date, narration, source, reviewed) VALUES (?, ?, ?, 1)",
    )
    .bind(m.date.to_string())
    .bind(note)
    .bind(Source::Manual.to_string())
    .execute(&mut *db)
    .await?
    .last_insert_rowid();
    for (account_id, amount) in [(from.id, flow), (other.id, -flow)] {
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency, origin) VALUES \
             (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(account_id)
        .bind(amount.to_string())
        .bind(m.currency.to_string())
        .bind(Origin::Manual.to_string())
        .execute(&mut *db)
        .await?;
    }
    let after = events::snapshot(&mut db, id).await?;
    events::record(&mut db, id, EventKind::Entered, &Change { before: None, after }).await?;
    gated(db, before).await?;
    Ok(id)
}
