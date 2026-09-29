//! What the review queue writes: an edit or split of a transaction's legs in
//! place, a confirmation, and a hand-entered transaction. Each commits only
//! with its `transaction_events` row and only if it breaks no balance a
//! statement, 天天記帳 or a count vouches for. Refusals are worded for the page
//! that shows them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger::{
    accounts::{in_subtree, Chart},
    journal::UNVERIFIED_TAG,
    model::Source,
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    assertions::{self, AssertionSource},
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
/// 未對帳 is re-marked for legs the edit moves; see `remark_unverified`.
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

/// Commits `db` unless the write broke a balance a statement or a count
/// vouches for; then it says which one, and what to do, in the reader's
/// words. A 天天記帳 closing it contradicts is only that app's sum of what
/// was logged there, so a record added later supersedes it instead.
async fn gated(mut db: sqlx::Transaction<'_, sqlx::Sqlite>, before: Vec<Mismatch>) -> Result<()> {
    let after = check::check(&mut db).await?;
    let (tiantian, broken): (Vec<&Mismatch>, Vec<&Mismatch>) =
        after.mismatches.iter().filter(|m| !before.contains(m)).partition(
            |m| matches!(&m.figure, Figure::Balance(a) if a.source == AssertionSource::Tiantian),
        );
    if let Some(broken) = broken.first() {
        let refusal = refusal(&mut db, broken).await?;
        bail!(refusal);
    }
    for m in tiantian {
        if let Figure::Balance(a) = &m.figure {
            assertions::supersede(&mut db, a).await?;
        }
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
pub async fn edit(
    pool: &SqlitePool,
    statements: &StatementAccounts,
    id: i64,
    edit: &Edit,
) -> Result<()> {
    let mut db = pool.begin().await?;
    let before_check = baseline(&mut db).await?;
    live(&mut db, id).await?;
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

    // Legs now on another account, with the account each left (`None` for
    // one the edit adds).
    let mut moved: HashMap<i64, Option<&str>> = HashMap::new();
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
                if !same_account {
                    moved.insert(pid, held.get(&pid).map(|r| r.path.as_str()));
                }
            }
            None => {
                let pid = sqlx::query(
                    "INSERT INTO postings (transaction_id, account_id, amount, currency, origin) \
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(id)
                .bind(target.id)
                .bind(leg.amount.to_string())
                .bind(leg.currency.to_string())
                .bind(Origin::Manual.to_string())
                .execute(&mut *db)
                .await?
                .last_insert_rowid();
                moved.insert(pid, None);
            }
        }
    }
    let mut removed = Vec::new();
    for r in rows.iter().filter(|r| !kept.contains(&r.id)) {
        sqlx::query("DELETE FROM postings WHERE id = ?").bind(r.id).execute(&mut *db).await?;
        removed.push(r.path.as_str());
    }
    remark_unverified(&mut db, statements, id, &moved, &removed).await?;
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

/// Refuses a transaction that is not there to change.
async fn live(db: &mut SqliteConnection, id: i64) -> Result<()> {
    let deleted: Option<Option<String>> =
        sqlx::query_scalar("SELECT deleted_at FROM transactions WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?;
    match deleted {
        None => bail!("查無交易 {id}"),
        Some(Some(_)) => bail!("這筆已經刪除了"),
        Some(None) => Ok(()),
    }
}

/// Takes a transaction out of the ledger: its legs are removed, so it
/// counts toward nothing, and it leaves every list. The row stays, with the
/// event holding what it was, so its source still knows it and it can be put
/// back by hand. Held to the same balances as an edit.
pub async fn delete(pool: &SqlitePool, id: i64) -> Result<()> {
    let mut db = pool.begin().await?;
    let before_check = baseline(&mut db).await?;
    live(&mut db, id).await?;
    let before = events::snapshot(&mut db, id).await?;
    sqlx::query("DELETE FROM postings WHERE transaction_id = ?").bind(id).execute(&mut *db).await?;
    sqlx::query(
        "UPDATE transactions SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), updated_at = \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *db)
    .await?;
    let after = events::snapshot(&mut db, id).await?;
    events::record(&mut db, id, EventKind::Deleted, &Change { before: Some(before), after })
        .await?;
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

/// Takes back a 確認: the transaction waits in the queue again, as it
/// stands. Unconfirming one not reviewed changes nothing and records nothing.
pub async fn unconfirm(pool: &SqlitePool, id: i64) -> Result<()> {
    let mut db = pool.begin().await?;
    live(&mut db, id).await?;
    let before = events::snapshot(&mut db, id).await?;
    if !before.reviewed {
        return Ok(());
    }
    sqlx::query(
        "UPDATE transactions SET reviewed = 0, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = ?",
    )
    .bind(id)
    .execute(&mut *db)
    .await?;
    let after = events::snapshot(&mut db, id).await?;
    events::record(&mut db, id, EventKind::Unconfirmed, &Change { before: Some(before), after })
        .await?;
    db.commit().await?;
    Ok(())
}

async fn confirm_in(db: &mut SqliteConnection, id: i64) -> Result<()> {
    live(db, id).await?;
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

/// The last day the statements of the account at `path` cover, or `None` for
/// an account no statement covers.
async fn statements_through(db: &mut SqliteConnection, path: &str) -> Result<Option<NaiveDate>> {
    let through: Option<String> = sqlx::query_scalar(
        "SELECT max(b.period_end) FROM balance_assertion b JOIN accounts a ON a.id = b.account_id \
         WHERE b.source = 'statement' AND b.superseded_at IS NULL AND (a.path = ?1 OR substr(?1, \
         1, length(a.path) + 1) = a.path || ':')",
    )
    .bind(path)
    .fetch_one(db)
    .await?;
    through.map(|d| d.parse()).transpose().context("statement period end")
}

/// The accounts bank statements post to, as the chart names them: where the
/// 天天記帳 importer marks a record 未對帳 until a statement shows it, even on
/// an account whose first statement is still to come.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatementAccounts(BTreeSet<String>);

impl From<&Chart> for StatementAccounts {
    fn from(chart: &Chart) -> Self {
        let i = &chart.institution;
        i.accounts.values().chain([&i.primary, &i.settlement]).map(|a| &**a).collect()
    }
}

impl<'a> FromIterator<&'a str> for StatementAccounts {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        Self(iter.into_iter().filter(|a| !a.is_empty()).map(str::to_string).collect())
    }
}

/// Whether statements post to the account at `path`, and the last day those
/// imported so far cover. One with statements imported is one whatever the
/// chart says.
async fn statement_account(
    db: &mut SqliteConnection,
    statements: &StatementAccounts,
    path: &str,
) -> Result<(bool, Option<NaiveDate>)> {
    let through = statements_through(db, path).await?;
    let named = statements.0.iter().any(|root| in_subtree(path, root));
    Ok((named || through.is_some(), through))
}

/// Whether a leg on `path` dated `date` is one a bank statement will show
/// but has not yet: statements post to the account, and none imported covers
/// `date`. Such a leg waits as 未對帳, as an imported 天天記帳 record does,
/// for the next statement import to verify it; untagged, that import would
/// book the bank's line a second time. A leg dated inside a statement is
/// held to it by the balance check instead.
async fn awaits_statement(
    db: &mut SqliteConnection,
    statements: &StatementAccounts,
    path: &str,
    date: NaiveDate,
) -> Result<bool> {
    let (on_statements, through) = statement_account(db, statements, path).await?;
    Ok(on_statements && through.is_none_or(|through| date > through))
}

async fn tag_unverified(db: &mut SqliteConnection, posting_id: i64, on: bool) -> Result<()> {
    let sql = match on {
        true => {
            "UPDATE postings SET tags = CASE WHEN instr(',' || coalesce(tags, '') || ',', ',' || \
             ?1 || ',') > 0 THEN tags ELSE coalesce(tags || ',', '') || ?1 END WHERE id = ?2"
        }
        false => {
            "UPDATE postings SET tags = nullif(trim(replace(',' || tags || ',', ',' || ?1 || ',', \
             ','), ','), '') WHERE id = ?2"
        }
    };
    sqlx::query(sql).bind(UNVERIFIED_TAG).bind(posting_id).execute(db).await?;
    Ok(())
}

/// Re-marks 未對帳 after an edit. A leg left on its account keeps its mark,
/// whatever the edit: only a statement clears it. A leg moved onto an account
/// statements post to is marked as 記一筆 would mark it; one moved elsewhere
/// keeps what it had. The record's other legs carry its mark along with the
/// bank's, so once a leg leaves a statement account, moved or `removed`, and
/// no leg on one is marked, nothing is left to verify and no leg keeps it.
async fn remark_unverified(
    db: &mut SqliteConnection,
    statements: &StatementAccounts,
    id: i64,
    moved: &HashMap<i64, Option<&str>>,
    removed: &[&str],
) -> Result<()> {
    let date: String = sqlx::query_scalar("SELECT date FROM transactions WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *db)
        .await?;
    let date: NaiveDate = date.parse().context("transaction date")?;
    let legs: Vec<(i64, String)> = sqlx::query_as(
        "SELECT p.id, a.path FROM postings p JOIN accounts a ON a.id = p.account_id WHERE \
         p.transaction_id = ?",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    let mut left_statements = false;
    for path in removed {
        left_statements |= statement_account(db, statements, path).await?.0;
    }
    for (pid, path) in &legs {
        let Some(left) = moved.get(pid) else { continue };
        if let Some(left) = left {
            left_statements |= statement_account(db, statements, left).await?.0;
        }
        if statement_account(db, statements, path).await?.0 {
            let awaits = awaits_statement(db, statements, path, date).await?;
            tag_unverified(db, *pid, awaits).await?;
        }
    }
    if !left_statements {
        return Ok(());
    }
    let marked: Vec<String> = sqlx::query_scalar(
        "SELECT a.path FROM postings p JOIN accounts a ON a.id = p.account_id WHERE \
         p.transaction_id = ? AND instr(',' || coalesce(p.tags, '') || ',', ',' || ? || ',') > 0",
    )
    .bind(id)
    .bind(UNVERIFIED_TAG)
    .fetch_all(&mut *db)
    .await?;
    let mut waits = false;
    for path in &marked {
        waits |= statement_account(db, statements, path).await?.0;
    }
    if !waits {
        for (pid, _) in &legs {
            tag_unverified(db, *pid, false).await?;
        }
    }
    Ok(())
}

/// Books a hand-entered transaction, reviewed. Returns its id.
pub async fn enter(pool: &SqlitePool, statements: &StatementAccounts, m: &Manual) -> Result<i64> {
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
    let counter = m.category.as_ref().or(m.counter.as_ref()).expect("checked above");
    for (path, account_id, amount) in [(&m.account, from.id, flow), (counter, other.id, -flow)] {
        let tags =
            awaits_statement(&mut db, statements, path, m.date).await?.then_some(UNVERIFIED_TAG);
        sqlx::query(
            "INSERT INTO postings (transaction_id, account_id, amount, currency, tags, origin) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(account_id)
        .bind(amount.to_string())
        .bind(m.currency.to_string())
        .bind(tags)
        .bind(Origin::Manual.to_string())
        .execute(&mut *db)
        .await?;
    }
    let after = events::snapshot(&mut db, id).await?;
    events::record(&mut db, id, EventKind::Entered, &Change { before: None, after }).await?;
    gated(db, before).await?;
    Ok(id)
}
