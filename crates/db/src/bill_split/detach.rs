//! Taking lines out of a split. Nothing imported is ever deleted.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::{SqliteConnection, SqlitePool};

use super::{
    linking::{record, LinkLeg},
    read::{held, line, load, Leg},
    Books, Role, Share, Split,
};
use crate::{
    events::EventKind,
    import::{ensure_account, Origin},
    query::AccountType,
    review::{self, Edit, LegEdit},
};

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
        let mut legs: Vec<LegEdit> = stays.iter().map(|l| LegEdit::from(*l)).collect();
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
    let legs = shares.iter().map(|s| LinkLeg::from(*s)).collect();
    record(db, split, transaction, EventKind::Detached, legs).await?;
    let wrote_off = shares.iter().any(|s| s.role == Role::WriteOff);
    let rewrote = shares.iter().any(|s| s.rewrote);
    match (wrote_off, rewrote) {
        (true, _) => review::delete_in(db, transaction).await,
        (false, true) => {
            // The last split to let go of a shared leg returns it.
            for share in &shares {
                sqlx::query("UPDATE share SET rewrote = 1 WHERE posting_id = ?")
                    .bind(share.posting_id)
                    .execute(&mut *db)
                    .await?;
            }
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

/// Deletes a split: every line detaches, none is deleted. Each detach's event
/// keeps the split's id and title.
pub async fn delete(pool: &SqlitePool, books: &Books, split_id: i64) -> Result<()> {
    let mut db = pool.begin().await?;
    let before = review::baseline(&mut db).await?;
    let split = load(&mut db, split_id).await?;
    let transactions: BTreeSet<i64> = split.shares.iter().map(|s| s.transaction_id).collect();
    for transaction in transactions {
        detach_in(&mut db, books, &split, transaction).await?;
    }
    sqlx::query("DELETE FROM split WHERE id = ?").bind(split_id).execute(&mut *db).await?;
    review::gated(db, before).await
}
