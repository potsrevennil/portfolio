//! Tying a line's legs to a split: which legs it can take, and in what role.
//! Links change no balance.

use anyhow::{bail, Context, Result};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::SqliteConnection;

use super::{
    model::{allocate, opposes},
    read::{Leg, Line, Owners},
    Books, Role, Shape, Share, Split,
};
use crate::{
    events::{self, EventKind},
    query::AccountType,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Repayment {
    /// The split account a 未分類 line's leg moves to, from the user or a
    /// mapping: the payee doesn't say. `None` for a line already on one.
    pub account: Option<String>,
    /// How much of it repays this split; all it can where `None`.
    pub amount: Option<Decimal>,
}

/// The payload of [`EventKind::Attached`] and [`EventKind::Detached`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Link {
    split_id: i64,
    title: String,
    legs: Vec<LinkLeg>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct LinkLeg {
    pub posting_id: i64,
    pub role: Role,
    pub person: Option<i64>,
    pub amount: Decimal,
    pub rewrote: bool,
}

impl From<&Share> for LinkLeg {
    fn from(s: &Share) -> Self {
        LinkLeg {
            posting_id: s.posting_id,
            role: s.role,
            person: s.person,
            amount: s.amount,
            rewrote: s.rewrote,
        }
    }
}

/// One split being added to, with what deciding that needs.
pub(super) struct Linking<'a> {
    pub split: &'a Split,
    pub owners: &'a Owners,
    pub books: &'a Books,
}

impl Linking<'_> {
    fn refuse_held(&self, line: &Line) -> Result<()> {
        match self.split.holds(line.id) {
            true => bail!("這筆已經在「{}」裡", self.split.title),
            false => Ok(()),
        }
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
                Shape::PerPerson => match self.owners.by_account.get(&leg.account) {
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
    pub fn repays(&self, line: &Line) -> Result<bool> {
        let owed = self.split.owed();
        Ok(self.takes(line)?.iter().any(|(leg, person)| {
            let against = match person {
                Some(person) => owed.get(&(*person, leg.currency)).copied().unwrap_or_default(),
                None => {
                    let same = self.split.shares.iter().filter(|s| s.currency == leg.currency);
                    same.map(|s| s.amount).sum()
                }
            };
            opposes(leg.left(), against)
        }))
    }

    /// Links `line`'s split legs as shares; with none, its own expense legs, as
    /// your share paid outside the split accounts. A 未分類 leg says nothing
    /// yet.
    pub async fn shares(
        &self,
        db: &mut SqliteConnection,
        line: &Line,
        rewrote: bool,
    ) -> Result<()> {
        let taken = self.takes(line)?;
        if taken.is_empty() {
            self.refuse_held(line)?;
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
        let legs = targets.into_iter().map(|(leg, person)| LinkLeg {
            posting_id: leg.posting_id,
            role,
            person,
            amount: leg.left(),
            rewrote,
        });
        link(db, self.split, line.id, legs.collect()).await
    }

    /// Links `line`'s split legs as repayments, or only its leg on
    /// `how.account`. A netted split has no one to cap them against; a
    /// per-person one takes what each owner still owes here. What a leg
    /// pays beyond that stays on the account, unallocated.
    pub async fn repayments(
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
                    // An amount the caller named is theirs to get right.
                    if how.amount.is_none() && !opposes(leg.left(), *still) {
                        continue;
                    }
                    let amount = allocate(leg.left(), Some(*still), how.amount)?;
                    *still += amount;
                    amount
                }
                None => allocate(leg.left(), None, how.amount)?,
            };
            legs.push(LinkLeg {
                posting_id: leg.posting_id,
                role: Role::Repayment,
                person,
                amount,
                rewrote,
            });
        }
        if legs.is_empty() {
            self.refuse_held(line)?;
        }
        if legs.is_empty() {
            bail!("這筆不是「{}」的還款：沒有人的欠款對得上", self.split.title);
        }
        link(db, self.split, line.id, legs).await
    }
}

/// Links `legs` to `split`: each its own row, all one event.
pub(super) async fn link(
    db: &mut SqliteConnection,
    split: &Split,
    transaction: i64,
    legs: Vec<LinkLeg>,
) -> Result<()> {
    for leg in &legs {
        sqlx::query(
            "INSERT INTO share (split_id, posting_id, role, person_id, amount, rewrote)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(split.id)
        .bind(leg.posting_id)
        .bind(leg.role.to_string())
        .bind(leg.person)
        .bind(leg.amount.to_string())
        .bind(leg.rewrote)
        .execute(&mut *db)
        .await
        .with_context(|| format!("linking posting {} to split {}", leg.posting_id, split.id))?;
    }
    record(db, split, transaction, EventKind::Attached, legs).await
}

pub(super) async fn record(
    db: &mut SqliteConnection,
    split: &Split,
    transaction: i64,
    kind: EventKind,
    legs: Vec<LinkLeg>,
) -> Result<()> {
    let link = Link { split_id: split.id, title: split.title.clone(), legs };
    events::record(db, transaction, kind, &link).await
}
