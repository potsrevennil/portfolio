//! What changes a split: creating it, gathering lines, attaching one, writing
//! off what is left. Links change no balance; where an operation also writes
//! legs it goes through the review queue's edit, so it is logged in
//! `transaction_events` and held to the balance gate. Refusals are worded for
//! the page that shows them.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use super::{
    linking::{link, LinkLeg, Linking, Repayment},
    mix::{Asked, Uses},
    model::named,
    read::{line, load, Leg, Line, Owners},
    Books, Role, Shape, Status, BAD_DEBT,
};
use crate::{
    import::{ensure_account, Origin},
    review::{self, Edit, LegEdit, Manual},
};

/// How a transaction joins a split.
#[derive(Clone, Debug, PartialEq)]
pub enum Attach {
    /// As new shares: its legs as they stand, or as this edit leaves them.
    Share(Option<Edit>),
    Repayment(Repayment),
}

/// Puts what a split account still owes on `Expenses:Bad-Debt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteOff {
    pub account: String,
    pub currency: Currency,
    pub date: NaiveDate,
    /// All that is owed here where `None`.
    pub amount: Option<Decimal>,
    pub note: Option<String>,
}

impl From<&Leg> for LegEdit {
    fn from(l: &Leg) -> Self {
        LegEdit {
            posting_id: Some(l.posting_id),
            account: l.account.clone(),
            amount: l.amount,
            currency: l.currency,
        }
    }
}

/// Creates a split. A netted one's account is created if new; it must not mix
/// with `Split:X:A` accounts.
pub async fn create(pool: &SqlitePool, books: &Books, title: &str, shape: &Shape) -> Result<i64> {
    let title = named(title, "分帳要有名")?;
    let mut db = pool.begin().await?;
    let account = match shape {
        Shape::PerPerson => None,
        Shape::Netted { account } => {
            if !books.holds(account) {
                bail!("「{account}」不是分帳底下的帳戶");
            }
            Uses::load(&mut db, books).await?.check(account, Asked::Netted)?;
            Some(ensure_account(&mut db, &books.labels, account).await?)
        }
    };
    let kind = match shape {
        Shape::PerPerson => "per_person",
        Shape::Netted { .. } => "netted",
    };
    let id = sqlx::query("INSERT INTO split (title, kind, account_id) VALUES (?, ?, ?)")
        .bind(title)
        .bind(kind)
        .bind(account)
        .execute(&mut *db)
        .await?
        .last_insert_rowid();
    db.commit().await?;
    Ok(id)
}

pub async fn rename(pool: &SqlitePool, id: i64, title: &str) -> Result<()> {
    let title = named(title, "分帳要有名")?;
    let renamed = sqlx::query("UPDATE split SET title = ? WHERE id = ?")
        .bind(title)
        .bind(id)
        .execute(pool)
        .await?;
    match renamed.rows_affected() {
        0 => bail!("查無分帳 {id}"),
        _ => Ok(()),
    }
}

/// Groups existing transactions into a split as they stand: their legs and
/// every balance stay exactly as they are. Money moving is a repayment where it
/// opposes what someone owes here by then, and otherwise a share: paying out
/// for a friend looks just like repaying one. All of them or none.
pub async fn gather(
    pool: &SqlitePool,
    books: &Books,
    split_id: i64,
    transactions: &[i64],
) -> Result<()> {
    let mut db = pool.begin().await?;
    let split = load(&mut db, split_id).await?;
    let owners = Owners::load(&mut db).await?;
    let ids: BTreeSet<i64> = transactions.iter().copied().collect();
    let mut lines = Vec::new();
    for id in ids {
        lines.push(line(&mut db, id).await?);
    }
    lines.sort_by_key(|l| (l.date, l.id));
    // Booked shares first: a repayment may be older than what it repays.
    let (moving, booked): (Vec<Line>, Vec<Line>) =
        lines.into_iter().partition(|l| l.role(books) == Role::Repayment);
    let linking = Linking { split: &split, owners: &owners, books };
    for line in &booked {
        linking.shares(&mut db, line, false).await?;
    }
    for line in &moving {
        let split = load(&mut db, split_id).await?;
        let linking = Linking { split: &split, owners: &owners, books };
        if linking.repays(line)? {
            linking.repayments(&mut db, line, &Repayment::default(), false).await?;
        } else {
            linking.shares(&mut db, line, false).await?;
        }
    }
    db.commit().await?;
    Ok(())
}

/// Moves a 未分類 line's one uncategorized leg onto `account`.
async fn repoint(
    db: &mut SqliteConnection,
    books: &Books,
    line: &Line,
    account: &str,
) -> Result<()> {
    let is_uncategorized = |l: &&Leg| {
        l.account == books.uncategorized_income || l.account == books.uncategorized_expense
    };
    let mut uncategorized = line.legs.iter().filter(is_uncategorized);
    let (Some(leg), None) = (uncategorized.next(), uncategorized.next()) else {
        bail!("這筆要有一筆未分類，才能改成還款");
    };
    let legs = line
        .legs
        .iter()
        .map(|l| match l.posting_id == leg.posting_id {
            true => LegEdit { account: account.to_string(), ..l.into() },
            false => l.into(),
        })
        .collect();
    let edit = Edit { narration: line.narration.clone(), legs, confirm: false };
    review::edit_in(db, &books.statements, line.id, &edit, Origin::Manual).await
}

/// Adds one transaction to an open or partial split. `Attach::Share` with an
/// edit and a repayment moved off 未分類 write legs, so these are held to the
/// balance gate, and detaching them returns the line to 未分類. The rest are
/// only links, with nothing for the gate to hold.
pub async fn attach(
    pool: &SqlitePool,
    books: &Books,
    split_id: i64,
    transaction: i64,
    how: &Attach,
) -> Result<()> {
    let mut db = pool.begin().await?;
    let split = load(&mut db, split_id).await?;
    if split.status() == Status::Settled {
        bail!("「{}」已經結清了", split.title);
    }
    let owners = Owners::load(&mut db).await?;
    let linking = Linking { split: &split, owners: &owners, books };
    let found = line(&mut db, transaction).await?;
    let target = match how {
        Attach::Share(_) => None,
        Attach::Repayment(repayment) => repayment
            .account
            .as_deref()
            .filter(|account| !found.legs.iter().any(|l| l.account == *account)),
    };
    match (how, target) {
        (Attach::Share(Some(edit)), _) => {
            let before = review::baseline(&mut db).await?;
            review::edit_in(&mut db, &books.statements, transaction, edit, Origin::Manual).await?;
            let found = line(&mut db, transaction).await?;
            linking.shares(&mut db, &found, true).await?;
            review::gated(db, before).await
        }
        (Attach::Share(None), _) => {
            linking.shares(&mut db, &found, false).await?;
            Ok(db.commit().await?)
        }
        (Attach::Repayment(repayment), Some(account)) => {
            let fits = match &split.shape {
                Shape::PerPerson => owners.by_account.contains_key(account),
                Shape::Netted { account: netted } => netted == account,
            };
            if !fits {
                bail!("「{account}」不是「{}」的人或群組的帳戶", split.title);
            }
            let before = review::baseline(&mut db).await?;
            repoint(&mut db, books, &found, account).await?;
            let found = line(&mut db, transaction).await?;
            linking.repayments(&mut db, &found, repayment, true).await?;
            review::gated(db, before).await
        }
        (Attach::Repayment(repayment), None) => {
            linking.repayments(&mut db, &found, repayment, false).await?;
            Ok(db.commit().await?)
        }
    }
}

/// Books a write-off as its own transaction, reviewed. Only what they owe you
/// can be written off; a payable is yours to pay. Returns its id.
pub async fn write_off(
    pool: &SqlitePool,
    books: &Books,
    split_id: i64,
    w: &WriteOff,
) -> Result<i64> {
    let mut db = pool.begin().await?;
    let before = review::baseline(&mut db).await?;
    let split = load(&mut db, split_id).await?;
    let (person, owed) = match &split.shape {
        Shape::Netted { account } if *account == w.account => {
            (None, split.balance.get(&w.currency).copied().unwrap_or_default())
        }
        Shape::Netted { account } => bail!("這個分帳的帳戶是「{account}」"),
        Shape::PerPerson => {
            let owners = Owners::load(&mut db).await?;
            let person = owners
                .by_account
                .get(&w.account)
                .copied()
                .with_context(|| format!("「{}」還沒指定是誰的帳", w.account))?;
            let owed = split.owed().get(&(person, w.currency)).copied().unwrap_or_default();
            (Some(person), owed)
        }
    };
    if owed <= Decimal::ZERO {
        bail!("「{}」沒有對方欠你的 {} 可以算冇數", split.title, w.currency);
    }
    let amount = w.amount.unwrap_or(owed);
    if amount <= Decimal::ZERO || amount > owed {
        bail!("冇數的金額要在 0 到 {owed} 之間");
    }
    ensure_account(&mut db, &books.labels, BAD_DEBT).await?;
    let manual = Manual {
        date: w.date,
        amount,
        currency: w.currency,
        account: w.account.clone(),
        category: Some(BAD_DEBT.to_string()),
        counter: None,
        note: w.note.clone(),
    };
    let id = review::enter_in(&mut db, &books.statements, &manual).await?;
    let posting = line(&mut db, id)
        .await?
        .legs
        .iter()
        .find(|l| l.account == w.account)
        .map(|l| l.posting_id)
        .context("a write-off's split leg")?;
    let leg = LinkLeg {
        posting_id: posting,
        role: Role::WriteOff,
        person,
        amount: -amount,
        rewrote: false,
    };
    link(&mut db, &split, id, vec![leg]).await?;
    review::gated(db, before).await?;
    Ok(id)
}
