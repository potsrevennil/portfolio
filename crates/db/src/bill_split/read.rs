//! How the engine reads the books: splits with their shares, lines with what
//! splits hold of them, and who owns which account. Writes nothing.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqliteConnection;

use super::{Books, Role, Shape, Share, Split};
use crate::query::{account_balance, field, AccountType};

/// A leg a split holds, for the queue to refuse editing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    pub posting_id: i64,
    pub title: String,
}

/// The legs of a transaction that a split holds.
pub(crate) async fn held(db: &mut SqliteConnection, transaction: i64) -> Result<Vec<Held>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT r.posting_id, s.title FROM share r
         JOIN split s ON s.id = r.split_id
         JOIN postings p ON p.id = r.posting_id WHERE p.transaction_id = ? ORDER BY r.id",
    )
    .bind(transaction)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(posting_id, title)| Held { posting_id, title }).collect())
}

const SPLIT_ROWS: &str = "SELECT s.id, s.title, s.kind, a.path FROM split s
    LEFT JOIN accounts a ON a.id = s.account_id";

#[derive(sqlx::FromRow)]
struct SplitRow {
    id: i64,
    title: String,
    kind: String,
    path: Option<String>,
}

impl SplitRow {
    fn shape(&self) -> Result<Shape> {
        match (self.kind.as_str(), &self.path) {
            ("netted", Some(account)) => Ok(Shape::Netted { account: account.clone() }),
            ("per_person", None) => Ok(Shape::PerPerson),
            (kind, path) => bail!("split {} is {kind} on {path:?}", self.id),
        }
    }

    /// With its shares and, if netted, its account's balance.
    async fn split(self, db: &mut SqliteConnection, shares: Vec<ShareRow>) -> Result<Split> {
        let shape = self.shape()?;
        let balance = match &shape {
            Shape::Netted { account } => account_balance(db, account, None).await?,
            Shape::PerPerson => BTreeMap::new(),
        };
        Ok(Split {
            id: self.id,
            title: self.title,
            shape,
            shares: shares.into_iter().map(Share::try_from).collect::<Result<_>>()?,
            balance,
        })
    }
}

const SHARE_ROWS: &str = "SELECT r.split_id, r.id, r.posting_id, p.transaction_id, t.date,
        t.narration, r.role, r.person_id, a.path, r.amount, p.currency,
        p.amount AS posting_amount, r.rewrote != 0 AS rewrote
    FROM share r
    JOIN postings p ON p.id = r.posting_id
    JOIN transactions t ON t.id = p.transaction_id
    JOIN accounts a ON a.id = p.account_id";

#[derive(sqlx::FromRow)]
struct ShareRow {
    split_id: i64,
    id: i64,
    posting_id: i64,
    transaction_id: i64,
    date: String,
    narration: Option<String>,
    role: String,
    person_id: Option<i64>,
    path: String,
    amount: String,
    currency: String,
    posting_amount: String,
    rewrote: bool,
}

impl TryFrom<ShareRow> for Share {
    type Error = anyhow::Error;

    fn try_from(r: ShareRow) -> Result<Self> {
        Ok(Share {
            id: r.id,
            posting_id: r.posting_id,
            transaction_id: r.transaction_id,
            date: field("transaction date", &r.date)?,
            narration: r.narration,
            role: field("share role", &r.role)?,
            person: r.person_id,
            account: r.path,
            amount: field("share amount", &r.amount)?,
            currency: field("currency", &r.currency)?,
            posting_amount: field("posting amount", &r.posting_amount)?,
            rewrote: r.rewrote,
        })
    }
}

pub(super) async fn load(db: &mut SqliteConnection, id: i64) -> Result<Split> {
    let row: SplitRow = sqlx::query_as(&format!("{SPLIT_ROWS} WHERE s.id = ?"))
        .bind(id)
        .fetch_optional(&mut *db)
        .await?
        .with_context(|| format!("查無分帳 {id}"))?;
    let shares: Vec<ShareRow> =
        sqlx::query_as(&format!("{SHARE_ROWS} WHERE r.split_id = ? ORDER BY t.date, r.id"))
            .bind(id)
            .fetch_all(&mut *db)
            .await?;
    row.split(db, shares).await
}

/// Every split, oldest first.
pub(super) async fn all(db: &mut SqliteConnection) -> Result<Vec<Split>> {
    let rows: Vec<SplitRow> =
        sqlx::query_as(&format!("{SPLIT_ROWS} ORDER BY s.id")).fetch_all(&mut *db).await?;
    let shares: Vec<ShareRow> =
        sqlx::query_as(&format!("{SHARE_ROWS} ORDER BY t.date, r.id")).fetch_all(&mut *db).await?;
    let mut by_split: BTreeMap<i64, Vec<ShareRow>> = BTreeMap::new();
    for share in shares {
        by_split.entry(share.split_id).or_default().push(share);
    }
    let mut splits = Vec::with_capacity(rows.len());
    for row in rows {
        let shares = by_split.remove(&row.id).unwrap_or_default();
        splits.push(row.split(db, shares).await?);
    }
    Ok(splits)
}

/// One leg of a line, with how much of it splits hold already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Leg {
    pub posting_id: i64,
    pub account: String,
    pub account_type: AccountType,
    pub amount: Decimal,
    pub currency: Currency,
    pub allocated: Decimal,
}

impl Leg {
    pub fn left(&self) -> Decimal { self.amount - self.allocated }
}

/// A transaction as the engine reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Line {
    pub id: i64,
    pub date: NaiveDate,
    pub narration: Option<String>,
    pub legs: Vec<Leg>,
}

/// The legs of the transactions `chosen` picks, a condition on their id with
/// one parameter.
async fn legs_of(
    db: &mut SqliteConnection,
    chosen: &str,
    param: i64,
) -> Result<BTreeMap<i64, Vec<Leg>>> {
    let rows: Vec<(i64, i64, String, String, String, String)> = sqlx::query_as(&format!(
        "SELECT p.transaction_id, p.id, a.path, a.type, p.amount, p.currency FROM postings p
         JOIN accounts a ON a.id = p.account_id WHERE p.transaction_id {chosen} ORDER BY p.id"
    ))
    .bind(param)
    .fetch_all(&mut *db)
    .await?;
    let links: Vec<(i64, String)> = sqlx::query_as(&format!(
        "SELECT r.posting_id, r.amount FROM share r JOIN postings p ON p.id = r.posting_id
         WHERE p.transaction_id {chosen}"
    ))
    .bind(param)
    .fetch_all(&mut *db)
    .await?;
    let mut allocated: BTreeMap<i64, Decimal> = BTreeMap::new();
    for (posting, amount) in &links {
        let amount: Decimal = field("share amount", amount)?;
        *allocated.entry(*posting).or_default() += amount;
    }
    let mut out: BTreeMap<i64, Vec<Leg>> = BTreeMap::new();
    for (transaction, posting_id, account, account_type, amount, currency) in rows {
        out.entry(transaction).or_default().push(Leg {
            posting_id,
            account,
            account_type: field("account type", &account_type)?,
            amount: field("posting amount", &amount)?,
            currency: field("currency", &currency)?,
            allocated: allocated.get(&posting_id).copied().unwrap_or_default(),
        });
    }
    Ok(out)
}

/// A transaction still in the ledger.
pub(super) async fn line(db: &mut SqliteConnection, id: i64) -> Result<Line> {
    let (date, narration, deleted): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT date, narration, deleted_at FROM transactions WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *db)
            .await?
            .with_context(|| format!("查無交易 {id}"))?;
    if deleted.is_some() {
        bail!("這筆已經刪除了");
    }
    let legs = legs_of(db, "= ?", id).await?.remove(&id).unwrap_or_default();
    Ok(Line { id, date: field("transaction date", &date)?, narration, legs })
}

/// Every line touching a person's accounts, oldest first.
pub(super) async fn lines_of(db: &mut SqliteConnection, person: i64) -> Result<Vec<Line>> {
    const THEIRS: &str = "IN (SELECT q.transaction_id FROM postings q
        JOIN person_account o ON o.account_id = q.account_id WHERE o.person_id = ?)";
    let headers: Vec<(i64, String, Option<String>)> = sqlx::query_as(&format!(
        "SELECT t.id, t.date, t.narration FROM transactions t WHERE t.id {THEIRS}
         ORDER BY t.date, t.id"
    ))
    .bind(person)
    .fetch_all(&mut *db)
    .await?;
    let mut legs = legs_of(db, THEIRS, person).await?;
    headers
        .into_iter()
        .map(|(id, date, narration)| {
            Ok(Line {
                id,
                date: field("transaction date", &date)?,
                narration,
                legs: legs.remove(&id).unwrap_or_default(),
            })
        })
        .collect()
}

impl Line {
    /// A line whose split legs meet only money moving can be a repayment.
    /// Anything booked to a category or equity, or moved between split
    /// accounts, is a share: the friend who fronted a dinner has a negative
    /// one.
    pub fn role(&self, books: &Books) -> Role {
        let others = || self.legs.iter().filter(|l| !books.split_accounts.covers(&l.account));
        let booked = others().any(|l| {
            matches!(
                l.account_type,
                AccountType::Income | AccountType::Expense | AccountType::Equity
            )
        });
        match !booked && others().next().is_some() {
            true => Role::Repayment,
            false => Role::Share,
        }
    }
}

/// Who owns each split account, and the accounts netted by some split.
#[derive(Debug, Default)]
pub(super) struct Owners {
    pub by_account: BTreeMap<String, i64>,
    pub netted: BTreeSet<String>,
}

impl Owners {
    pub async fn load(db: &mut SqliteConnection) -> Result<Self> {
        let owners: Vec<(String, i64)> = sqlx::query_as(
            "SELECT a.path, o.person_id FROM person_account o JOIN accounts a ON a.id = \
             o.account_id",
        )
        .fetch_all(&mut *db)
        .await?;
        let netted: Vec<String> =
            sqlx::query_scalar("SELECT a.path FROM split s JOIN accounts a ON a.id = s.account_id")
                .fetch_all(&mut *db)
                .await?;
        Ok(Owners {
            by_account: owners.into_iter().collect(),
            netted: netted.into_iter().collect(),
        })
    }

    /// The accounts whose shape someone has settled.
    pub fn settled(&self) -> impl Iterator<Item = &String> {
        self.netted.iter().chain(self.by_account.keys())
    }

    /// A person's accounts, by path.
    pub fn of(&self, person: i64) -> Vec<String> {
        let theirs = self.by_account.iter().filter(|(_, owner)| **owner == person);
        theirs.map(|(path, _)| path.clone()).collect()
    }
}
