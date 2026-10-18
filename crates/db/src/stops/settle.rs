//! What the queue writes to settle a stop, each held to the review gate and
//! logged in `transaction_events`.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use ledger::labels::Labels;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use sqlx::SqlitePool;

use super::{
    store::{open_one, resolve},
    Pick, Stop,
};
use crate::{
    events::{self, Change, Detachment, EventKind, RecordPairing},
    import::{ensure_account, Origin},
    review::{self, Edit, LegEdit, PostingRow, StatementAccounts},
};

/// Picks where the record of stop `id` belongs: one of the lines offered, or
/// one of the accounts. A line picked for another open record cannot be
/// picked again: one line is one record.
pub async fn choose(pool: &SqlitePool, id: i64, pick: &Pick) -> Result<()> {
    let mut db = pool.begin().await?;
    let open = open_one(&mut db, id).await?;
    let (record, lines, accounts) = match &open.stop {
        Stop::RecordAmbiguous { record, lines } => (record, lines.as_slice(), &[][..]),
        Stop::RecordUnexplained { record, lines, accounts, .. } => {
            (record, lines.as_slice(), accounts.as_slice())
        }
        _ => bail!("這筆免揀"),
    };
    match pick {
        Pick::Line(line) => {
            let deleted: Option<String> =
                sqlx::query_scalar("SELECT deleted_at FROM transactions WHERE id = ?")
                    .bind(line)
                    .fetch_optional(&mut *db)
                    .await?
                    .flatten();
            if !lines.contains(line) || deleted.is_some() {
                bail!("這筆無佇會使配的明細內底");
            }
            let taken: Option<String> = sqlx::query_scalar(
                "SELECT subject FROM import_stop
                 WHERE cleared_at IS NULL AND json_extract(pick, '$.line') = ? AND id <> ?",
            )
            .bind(line)
            .bind(id)
            .fetch_optional(&mut *db)
            .await?;
            if taken.is_some() {
                bail!("這筆明細已經配予另外一筆紀錄");
            }
            let pairing = RecordPairing {
                record_ref: record.external_ref.clone(),
                date: record.date,
                amount: record.amount,
            };
            events::record(&mut db, *line, EventKind::Paired, &pairing).await?;
        }
        Pick::Account(account) if !accounts.contains(account) => bail!("袂使記佇這个帳戶"),
        Pick::Account(_) => {}
    }
    sqlx::query(
        "UPDATE import_stop SET pick = ?, resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?",
    )
    .bind(serde_json::to_string(pick)?)
    .bind(id)
    .execute(&mut *db)
    .await?;
    db.commit().await?;
    Ok(())
}

/// Splits the record of stop `id` in two through the clearing account, so
/// each bank's leg is a record of its own and can take its own date: the
/// legs on the other statement account move to a new transaction, and a
/// clearing leg on each side holds what is in transit. Returns the new
/// transaction's id.
pub async fn detach(pool: &SqlitePool, id: i64) -> Result<i64> {
    let mut db = pool.begin().await?;
    let before_check = review::baseline(&mut db).await?;
    let (transaction_id, moving, clearing) = match open_one(&mut db, id).await?.stop {
        Stop::SharedDeferral { transaction_id, other, clearing, .. } => {
            (transaction_id, vec![other], clearing)
        }
        Stop::TwoDates { transaction_id, dates, clearing } => {
            (transaction_id, dates.into_iter().skip(1).map(|(a, _)| a).collect(), clearing)
        }
        _ => bail!("這筆袂使拆"),
    };
    if clearing.is_empty() {
        bail!("mapping.toml 的 [institution] 無設 clearing（在途帳戶）");
    }
    review::live(&mut db, transaction_id).await?;
    let before = events::snapshot(&mut db, transaction_id).await?;
    let (moved, kept): (Vec<PostingRow>, Vec<PostingRow>) =
        review::postings(&mut db, transaction_id)
            .await?
            .into_iter()
            .partition(|l| moving.contains(&l.path));
    if moved.is_empty() || kept.is_empty() {
        bail!("這筆已經毋是兩个帳戶之間的轉帳");
    }

    let detached = sqlx::query(
        "INSERT INTO transactions (date, payee, narration, source, reviewed)
         SELECT date, payee, narration, source, reviewed FROM transactions WHERE id = ?",
    )
    .bind(transaction_id)
    .execute(&mut *db)
    .await?
    .last_insert_rowid();
    let mut in_transit: HashMap<Currency, Decimal> = HashMap::new();
    for leg in &moved {
        sqlx::query("UPDATE postings SET transaction_id = ? WHERE id = ?")
            .bind(detached)
            .bind(leg.id)
            .execute(&mut *db)
            .await?;
        let amount: Decimal = leg.amount.parse().context("posting amount")?;
        let currency: Currency = leg.currency.parse().context("posting currency")?;
        *in_transit.entry(currency).or_default() += amount;
    }
    let clearing_id = ensure_account(&mut db, &Labels::default(), &clearing).await?;
    for (currency, amount) in &in_transit {
        for (txn, amount) in [(transaction_id, *amount), (detached, -*amount)] {
            sqlx::query(
                "INSERT INTO postings (transaction_id, account_id, amount, currency, origin)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(txn)
            .bind(clearing_id)
            .bind(amount.to_string())
            .bind(currency.to_string())
            .bind(Origin::Manual.to_string())
            .execute(&mut *db)
            .await?;
        }
    }

    let after = events::snapshot(&mut db, transaction_id).await?;
    events::record(&mut db, transaction_id, EventKind::Edited, &Change {
        before: Some(before),
        after,
    })
    .await?;
    let after = events::snapshot(&mut db, detached).await?;
    let detachment = Detachment { from: transaction_id, before: None, after };
    events::record(&mut db, detached, EventKind::Detached, &detachment).await?;
    resolve(&mut db, id).await?;
    review::gated(db, before_check).await?;
    Ok(detached)
}

/// Moves the leg of stop `id`'s record to `to`, a sibling account whose
/// statement shows it: an edit, held to the balances like any other.
pub async fn move_leg(
    pool: &SqlitePool,
    statements: &StatementAccounts,
    id: i64,
    to: &str,
) -> Result<()> {
    let mut db = pool.begin().await?;
    let before_check = review::baseline(&mut db).await?;
    let (transaction_id, account, amount, currency) =
        match open_one(&mut db, id).await?.stop {
            Stop::NotOnStatement {
                transaction_id, account, amount, currency, siblings, ..
            } if siblings.iter().any(|s| s == to) => (transaction_id, account, amount, currency),
            Stop::NotOnStatement { .. } => bail!("袂使改記佇這个帳戶"),
            _ => bail!("這筆袂使改帳戶"),
        };
    let narration: Option<String> =
        sqlx::query_scalar("SELECT narration FROM transactions WHERE id = ?")
            .bind(transaction_id)
            .fetch_one(&mut *db)
            .await?;
    let mut found = false;
    let mut edit = Edit { narration, legs: Vec::new(), confirm: false };
    for leg in review::postings(&mut db, transaction_id).await? {
        let (amount_here, currency_here): (Decimal, Currency) =
            (leg.amount.parse()?, leg.currency.parse()?);
        let this =
            !found && leg.path == account && (amount_here, currency_here) == (amount, currency);
        found |= this;
        edit.legs.push(LegEdit {
            posting_id: Some(leg.id),
            account: match this {
                true => to.to_string(),
                false => leg.path,
            },
            amount: amount_here,
            currency: currency_here,
        });
    }
    if !found {
        bail!("這筆已經改過，無佇原來的帳戶");
    }
    review::edit_in(&mut db, statements, transaction_id, &edit).await?;
    resolve(&mut db, id).await?;
    review::gated(db, before_check).await
}
