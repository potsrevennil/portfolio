//! What changes a split: gather, attach, detach, write-off. Links change no
//! balance; where an operation also writes legs it goes through the review
//! queue's edit, so it is logged in `transaction_events` and held to the
//! balance gate. Refusals are worded for the page that shows them.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use super::{
    mix::{Asked, Uses},
    model::allocate,
    Books, Role, Shape, Share, Split, Status, BAD_DEBT,
};
use crate::{
    events::{self, EventKind},
    import::{ensure_account, Origin},
    query::{field, AccountType},
    review::{self, Edit, LegEdit, Manual},
};

/// How a transaction joins a split.
#[derive(Clone, Debug, PartialEq)]
pub enum Attach {
    /// As new shares: its legs as they stand, or as this edit leaves them.
    Share(Option<Edit>),
    Repayment(Repayment),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repayment {
    /// The split account a 未分類 line's leg moves to, from the user or a
    /// mapping: the payee doesn't say. `None` for a line already on one.
    pub account: Option<String>,
    /// How much of it repays this split; all it can where `None`.
    pub amount: Option<Decimal>,
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

/// The payload of [`EventKind::Attached`] and [`EventKind::Detached`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub split_id: i64,
    pub title: String,
    pub legs: Vec<LinkLeg>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkLeg {
    pub posting_id: i64,
    pub role: Role,
    pub person: Option<i64>,
    pub amount: Decimal,
    pub rewrote: bool,
}

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

#[derive(sqlx::FromRow)]
struct SplitRow {
    id: i64,
    title: String,
    kind: String,
    path: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ShareRow {
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

/// An account's balance by currency, through `as_of` where given.
pub(super) async fn account_balance(
    db: &mut SqliteConnection,
    path: &str,
    as_of: Option<NaiveDate>,
) -> Result<BTreeMap<Currency, Decimal>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.amount, p.currency FROM postings p
         JOIN accounts a ON a.id = p.account_id
         JOIN transactions t ON t.id = p.transaction_id
         WHERE a.path = ?1 AND (?2 IS NULL OR t.date <= ?2)",
    )
    .bind(path)
    .bind(as_of.map(|d| d.to_string()))
    .fetch_all(db)
    .await?;
    let mut balance = BTreeMap::new();
    for (amount, currency) in rows {
        let amount: Decimal = field("posting amount", &amount)?;
        *balance.entry(field("currency", &currency)?).or_default() += amount;
    }
    Ok(balance)
}

pub(super) async fn load(db: &mut SqliteConnection, id: i64) -> Result<Split> {
    let row: SplitRow = sqlx::query_as(
        "SELECT s.id, s.title, s.kind, a.path FROM split s
         LEFT JOIN accounts a ON a.id = s.account_id WHERE s.id = ? AND s.deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&mut *db)
    .await?
    .with_context(|| format!("查無分帳 {id}"))?;
    let shares: Vec<ShareRow> = sqlx::query_as(
        "SELECT r.id, r.posting_id, p.transaction_id, t.date, t.narration, r.role, r.person_id,
                a.path, r.amount, p.currency, p.amount AS posting_amount, r.rewrote != 0 AS rewrote
         FROM share r
         JOIN postings p ON p.id = r.posting_id
         JOIN transactions t ON t.id = p.transaction_id
         JOIN accounts a ON a.id = p.account_id
         WHERE r.split_id = ? ORDER BY t.date, r.id",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    let (shape, balance) = match (row.kind.as_str(), row.path) {
        ("netted", Some(account)) => {
            let balance = account_balance(db, &account, None).await?;
            (Shape::Netted { account }, balance)
        }
        ("per_person", None) => (Shape::PerPerson, BTreeMap::new()),
        (kind, path) => bail!("split {id} is {kind} on {path:?}"),
    };
    Ok(Split {
        id: row.id,
        title: row.title,
        shape,
        shares: shares.into_iter().map(Share::try_from).collect::<Result<_>>()?,
        balance,
    })
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

pub(super) async fn line(db: &mut SqliteConnection, id: i64) -> Result<Line> {
    let (date, narration): (String, Option<String>) =
        sqlx::query_as("SELECT date, narration FROM transactions WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *db)
            .await?
            .with_context(|| format!("查無交易 {id}"))?;
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT p.id, a.path, a.type, p.amount, p.currency FROM postings p
         JOIN accounts a ON a.id = p.account_id WHERE p.transaction_id = ? ORDER BY p.id",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    let links: Vec<(i64, String)> = sqlx::query_as(
        "SELECT r.posting_id, r.amount FROM share r JOIN postings p ON p.id = r.posting_id
         WHERE p.transaction_id = ?",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    let mut allocated: BTreeMap<i64, Decimal> = BTreeMap::new();
    for (posting, amount) in &links {
        let amount: Decimal = field("share amount", amount)?;
        *allocated.entry(*posting).or_default() += amount;
    }
    let legs = rows
        .into_iter()
        .map(|(posting_id, account, account_type, amount, currency)| {
            Ok(Leg {
                posting_id,
                account,
                account_type: field("account type", &account_type)?,
                amount: field("posting amount", &amount)?,
                currency: field("currency", &currency)?,
                allocated: allocated.get(&posting_id).copied().unwrap_or_default(),
            })
        })
        .collect::<Result<_>>()?;
    Ok(Line { id, date: field("transaction date", &date)?, narration, legs })
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
pub(super) struct Owners {
    pub owners: BTreeMap<String, i64>,
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
        let netted: Vec<String> = sqlx::query_scalar(
            "SELECT a.path FROM split s JOIN accounts a ON a.id = s.account_id
             WHERE s.deleted_at IS NULL",
        )
        .fetch_all(&mut *db)
        .await?;
        Ok(Owners { owners: owners.into_iter().collect(), netted: netted.into_iter().collect() })
    }
}

/// One split being added to, with what deciding that needs.
struct Linking<'a> {
    split: &'a Split,
    owners: &'a Owners,
    books: &'a Books,
}

impl Linking<'_> {
    fn holds(&self, line: &Line) -> bool {
        self.split.shares.iter().any(|s| s.transaction_id == line.id)
    }

    /// The legs of `line` this split can still take, with their owner: not one
    /// it holds already, or that others have used up. Refuses a split account
    /// no one owns: the people list is explicit, never guessed.
    fn takes<'l>(&self, line: &'l Line) -> Result<Vec<(&'l Leg, Option<i64>)>> {
        let mut out = Vec::new();
        for leg in &line.legs {
            let here = self.split.shares.iter().any(|s| s.posting_id == leg.posting_id);
            if here || !self.books.split_accounts.covers(&leg.account) || leg.left().is_zero() {
                continue;
            }
            match &self.split.shape {
                Shape::Netted { account } if *account == leg.account => out.push((leg, None)),
                Shape::Netted { .. } => {}
                Shape::PerPerson => match self.owners.owners.get(&leg.account) {
                    Some(person) => out.push((leg, Some(*person))),
                    None if self.owners.netted.contains(&leg.account) => {}
                    None => bail!("「{}」還沒指定是誰的帳，先指定才能加進分帳", leg.account),
                },
            }
        }
        Ok(out)
    }

    /// Whether a leg of `line` runs against what is owed here so far, by its
    /// owner or, in a netted split, by the group.
    fn repays(&self, line: &Line) -> Result<bool> {
        let owed = self.split.owed();
        Ok(self.takes(line)?.iter().any(|(leg, person)| {
            let against = match person {
                Some(person) => owed.get(&(*person, leg.currency)).copied().unwrap_or_default(),
                None => {
                    let same = self.split.shares.iter().filter(|s| s.currency == leg.currency);
                    same.map(|s| s.amount).sum()
                }
            };
            !against.is_zero() && against.is_sign_negative() != leg.left().is_sign_negative()
        }))
    }

    /// Links `line`'s split legs as shares; with none, its own expense legs, as
    /// your share paid outside the split accounts. A 未分類 leg says nothing
    /// yet.
    async fn shares(&self, db: &mut SqliteConnection, line: &Line, rewrote: bool) -> Result<()> {
        let taken = self.takes(line)?;
        if taken.is_empty() && self.holds(line) {
            bail!("這筆已經在「{}」裡", self.split.title);
        }
        let (role, targets): (Role, Vec<(&Leg, Option<i64>)>) = match taken.is_empty() {
            false => (Role::Share, taken),
            true => {
                let own = line.legs.iter().filter(|l| {
                    l.account_type == AccountType::Expense
                        && !self.books.provisional.contains(&l.account)
                        && !l.left().is_zero()
                });
                (Role::Own, own.map(|l| (l, None)).collect())
            }
        };
        if targets.is_empty() {
            bail!("這筆沒有「{}」可以算的腳：先分好再加進來", self.split.title);
        }
        let mut legs = Vec::new();
        for (leg, person) in targets {
            let link =
                LinkLeg { posting_id: leg.posting_id, role, person, amount: leg.left(), rewrote };
            legs.push(insert_share(db, self.split.id, link).await?);
        }
        record(db, self.split, line.id, EventKind::Attached, legs).await
    }

    /// Links `line`'s split legs as repayments, or only its leg on
    /// `how.account`. A netted split has no one to cap them against; a
    /// per-person one takes what each owner still owes here. What a leg
    /// pays beyond that stays on the account, unallocated.
    async fn repayments(
        &self,
        db: &mut SqliteConnection,
        line: &Line,
        how: &Repayment,
        rewrote: bool,
    ) -> Result<()> {
        let taken: Vec<_> = self
            .takes(line)?
            .into_iter()
            .filter(|(leg, _)| how.account.as_deref().is_none_or(|a| a == leg.account))
            .collect();
        if how.amount.is_some() && taken.len() > 1 {
            bail!("這筆有幾個分帳的腳，不能只寫一個金額");
        }
        let mut owed = self.split.owed();
        let mut legs = Vec::new();
        for (leg, person) in taken {
            let amount = match person {
                Some(person) => {
                    let still = owed.entry((person, leg.currency)).or_default();
                    match allocate(leg.left(), *still, how.amount) {
                        Ok(amount) => {
                            *still += amount;
                            amount
                        }
                        // An amount the caller named and got wrong is theirs to fix.
                        Err(e) if how.amount.is_some() => return Err(e),
                        Err(_) => continue,
                    }
                }
                None => allocate(leg.left(), -leg.left(), how.amount)?,
            };
            let link = LinkLeg {
                posting_id: leg.posting_id,
                role: Role::Repayment,
                person,
                amount,
                rewrote,
            };
            legs.push(insert_share(db, self.split.id, link).await?);
        }
        if legs.is_empty() && self.holds(line) {
            bail!("這筆已經在「{}」裡", self.split.title);
        }
        if legs.is_empty() {
            bail!("這筆不是「{}」的還款：沒有人的欠款對得上", self.split.title);
        }
        record(db, self.split, line.id, EventKind::Attached, legs).await
    }
}

async fn insert_share(db: &mut SqliteConnection, split: i64, leg: LinkLeg) -> Result<LinkLeg> {
    sqlx::query(
        "INSERT INTO share (split_id, posting_id, role, person_id, amount, rewrote)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(split)
    .bind(leg.posting_id)
    .bind(leg.role.to_string())
    .bind(leg.person)
    .bind(leg.amount.to_string())
    .bind(leg.rewrote)
    .execute(db)
    .await
    .with_context(|| format!("linking posting {} to split {split}", leg.posting_id))?;
    Ok(leg)
}

async fn record(
    db: &mut SqliteConnection,
    split: &Split,
    transaction: i64,
    kind: EventKind,
    legs: Vec<LinkLeg>,
) -> Result<()> {
    let link = Link { split_id: split.id, title: split.title.clone(), legs };
    events::record(db, transaction, kind, &link).await
}

/// Creates a split. A netted one's account is created if new; it must not mix
/// with `Split:X:A` accounts.
pub async fn create(pool: &SqlitePool, books: &Books, title: &str, shape: &Shape) -> Result<i64> {
    let title = title.trim();
    if title.is_empty() {
        bail!("分帳要有名");
    }
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
    let title = title.trim();
    if title.is_empty() {
        bail!("分帳要有名");
    }
    let renamed = sqlx::query("UPDATE split SET title = ? WHERE id = ? AND deleted_at IS NULL")
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
        review::live(&mut db, id).await?;
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
        .map(|l| LegEdit {
            posting_id: Some(l.posting_id),
            account: match l.posting_id == leg.posting_id {
                true => account.to_string(),
                false => l.account.clone(),
            },
            amount: l.amount,
            currency: l.currency,
        })
        .collect();
    let edit = Edit { narration: line.narration.clone(), legs, confirm: false };
    review::edit_in(db, &books.statements, line.id, &edit, Origin::Manual).await
}

/// Adds one transaction to an open or partial split. `Attach::Share` with an
/// edit and a repayment moved off 未分類 write legs, so these are held to the
/// balance gate, and detaching them returns the line to 未分類.
pub async fn attach(
    pool: &SqlitePool,
    books: &Books,
    split_id: i64,
    transaction: i64,
    how: &Attach,
) -> Result<()> {
    let mut db = pool.begin().await?;
    let before = review::baseline(&mut db).await?;
    let split = load(&mut db, split_id).await?;
    if split.status() == Status::Settled {
        bail!("「{}」已經結清了", split.title);
    }
    let owners = Owners::load(&mut db).await?;
    let linking = Linking { split: &split, owners: &owners, books };
    review::live(&mut db, transaction).await?;
    let found = line(&mut db, transaction).await?;
    match how {
        Attach::Share(edit) => {
            if let Some(edit) = edit {
                review::edit_in(&mut db, &books.statements, transaction, edit, Origin::Manual)
                    .await?;
            }
            let found = line(&mut db, transaction).await?;
            linking.shares(&mut db, &found, edit.is_some()).await?;
        }
        Attach::Repayment(repayment) => {
            let target = repayment
                .account
                .as_deref()
                .filter(|account| !found.legs.iter().any(|l| l.account == *account));
            if let Some(account) = target {
                let fits = match &split.shape {
                    Shape::PerPerson => owners.owners.contains_key(account),
                    Shape::Netted { account: netted } => netted == account,
                };
                if !fits {
                    bail!("「{account}」不是「{}」的人或群組的帳戶", split.title);
                }
                repoint(&mut db, books, &found, account).await?;
            }
            let found = line(&mut db, transaction).await?;
            linking.repayments(&mut db, &found, repayment, target.is_some()).await?;
        }
    }
    review::gated(db, before).await
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
                .owners
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
    let link = LinkLeg {
        posting_id: posting,
        role: Role::WriteOff,
        person,
        amount: -amount,
        rewrote: false,
    };
    let leg = insert_share(&mut db, split.id, link).await?;
    record(&mut db, &split, id, EventKind::Attached, vec![leg]).await?;
    review::gated(db, before).await?;
    Ok(id)
}

/// Returns what `released` held to 未分類, and the line to the queue. Releasing
/// a share also returns the line's categories, since the engine wrote those
/// with it; a repayment only moved its own leg. The money legs stay, and so do
/// legs nothing here wrote or another split still holds. A line left with no
/// leg but these has nothing to return and keeps its legs.
async fn uncategorize(
    db: &mut SqliteConnection,
    books: &Books,
    transaction: i64,
    released: &[&Share],
) -> Result<()> {
    let found = line(db, transaction).await?;
    let held: BTreeSet<i64> = held(db, transaction).await?.iter().map(|h| h.posting_id).collect();
    let categories = released.iter().any(|s| s.role != Role::Repayment);
    let (folded, stays): (Vec<&Leg>, Vec<&Leg>) = found.legs.iter().partition(|l| {
        let booked = matches!(l.account_type, AccountType::Income | AccountType::Expense);
        !held.contains(&l.posting_id)
            && (released.iter().any(|s| s.posting_id == l.posting_id) || (categories && booked))
    });
    if stays.is_empty() || folded.is_empty() {
        Ok(())
    } else {
        let mut legs: Vec<LegEdit> = stays
            .iter()
            .map(|l| LegEdit {
                posting_id: Some(l.posting_id),
                account: l.account.clone(),
                amount: l.amount,
                currency: l.currency,
            })
            .collect();
        let mut nets: BTreeMap<Currency, (i64, Decimal)> = BTreeMap::new();
        for leg in &folded {
            nets.entry(leg.currency).or_insert((leg.posting_id, Decimal::ZERO)).1 += leg.amount;
        }
        for (currency, (posting_id, net)) in nets {
            if net.is_zero() {
                continue;
            }
            let account = match net.is_sign_positive() {
                true => &books.uncategorized_expense,
                false => &books.uncategorized_income,
            };
            ensure_account(db, &books.labels, account).await?;
            legs.push(LegEdit {
                posting_id: Some(posting_id),
                account: account.clone(),
                amount: net,
                currency,
            });
        }
        let edit = Edit { narration: found.narration.clone(), legs, confirm: false };
        review::edit_in(db, &books.statements, transaction, &edit, Origin::Fallback).await?;
        review::unconfirm_in(db, transaction).await
    }
}

/// Takes one transaction out of `split`. A line the engine wrote legs on
/// returns to 未分類; a line merely gathered keeps its legs. A write-off exists
/// only for this split, so it is taken back: its row stays, marked deleted,
/// with the event holding what it was. Nothing imported is ever deleted.
async fn detach_in(
    db: &mut SqliteConnection,
    books: &Books,
    split: &Split,
    transaction: i64,
) -> Result<()> {
    let shares: Vec<&Share> =
        split.shares.iter().filter(|s| s.transaction_id == transaction).collect();
    if shares.is_empty() {
        bail!("這筆不在「{}」裡", split.title);
    }
    for share in &shares {
        sqlx::query("DELETE FROM share WHERE id = ?").bind(share.id).execute(&mut *db).await?;
    }
    let legs = shares
        .iter()
        .map(|s| LinkLeg {
            posting_id: s.posting_id,
            role: s.role,
            person: s.person,
            amount: s.amount,
            rewrote: s.rewrote,
        })
        .collect();
    record(db, split, transaction, EventKind::Detached, legs).await?;
    let wrote_off = shares.iter().any(|s| s.role == Role::WriteOff);
    let rewrote = shares.iter().any(|s| s.rewrote);
    match (wrote_off, rewrote) {
        (true, _) => match held(db, transaction).await?.first() {
            Some(other) => bail!("這筆冇數也在分帳「{}」裡，先退出那邊", other.title),
            None => review::delete_in(db, transaction).await,
        },
        (false, true) => {
            // The last split to let go of a shared leg returns it.
            sqlx::query(
                "UPDATE share SET rewrote = 1 WHERE posting_id IN
                    (SELECT id FROM postings WHERE transaction_id = ?)",
            )
            .bind(transaction)
            .execute(&mut *db)
            .await?;
            uncategorize(db, books, transaction, &shares).await
        }
        (false, false) => Ok(()),
    }
}

pub async fn detach(
    pool: &SqlitePool,
    books: &Books,
    split_id: i64,
    transaction: i64,
) -> Result<()> {
    let mut db = pool.begin().await?;
    let before = review::baseline(&mut db).await?;
    let split = load(&mut db, split_id).await?;
    detach_in(&mut db, books, &split, transaction).await?;
    review::gated(db, before).await
}

/// Deletes a split: every line detaches, none is deleted.
pub async fn delete(pool: &SqlitePool, books: &Books, split_id: i64) -> Result<()> {
    let mut db = pool.begin().await?;
    let before = review::baseline(&mut db).await?;
    let split = load(&mut db, split_id).await?;
    let transactions: BTreeSet<i64> = split.shares.iter().map(|s| s.transaction_id).collect();
    for transaction in transactions {
        detach_in(&mut db, books, &split, transaction).await?;
    }
    sqlx::query("UPDATE split SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
        .bind(split_id)
        .execute(&mut *db)
        .await?;
    review::gated(db, before).await
}
