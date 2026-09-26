//! `transaction_events`: the append-only history of what the app changed on a
//! transaction. Every edit writes the whole transaction before and after, so
//! overwriting its rows in place loses nothing.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use strum_macros::{Display, EnumString};

use crate::import::Origin;

/// The `transaction_events.kind` vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, EnumString, Serialize, Deserialize)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    /// Entered by hand.
    Entered,
    Edited,
    /// Edited into more legs than it had.
    Split,
    Confirmed,
    /// Chosen as the record a statement line verifies.
    Paired,
    /// Taken out of the ledger: its legs are gone, `after` has none.
    Deleted,
}

/// A whole transaction as it stood: what `before` and `after` hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub date: NaiveDate,
    pub payee: Option<String>,
    pub narration: Option<String>,
    pub reviewed: bool,
    pub legs: Vec<SnapshotLeg>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotLeg {
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
    pub tags: Option<String>,
    pub origin: Option<Origin>,
}

/// The payload of every kind but [`EventKind::Paired`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub before: Option<Snapshot>,
    pub after: Snapshot,
}

/// The payload of [`EventKind::Paired`]: the statement line chosen for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pairing {
    pub statement_ref: String,
    pub date: NaiveDate,
    pub amount: Decimal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub at: String,
    pub kind: EventKind,
    /// JSON, as stored.
    pub payload: String,
}

#[derive(sqlx::FromRow)]
struct HeaderRow {
    date: String,
    payee: Option<String>,
    narration: Option<String>,
    reviewed: bool,
}

#[derive(sqlx::FromRow)]
struct LegRow {
    path: String,
    amount: String,
    currency: String,
    tags: Option<String>,
    origin: Option<String>,
}

impl TryFrom<LegRow> for SnapshotLeg {
    type Error = anyhow::Error;

    fn try_from(r: LegRow) -> Result<Self> {
        Ok(SnapshotLeg {
            amount: r.amount.parse().with_context(|| format!("posting amount {:?}", r.amount))?,
            currency: r.currency.parse().with_context(|| format!("currency {:?}", r.currency))?,
            origin: r.origin.map(|o| o.parse()).transpose().context("posting origin")?,
            account: r.path,
            tags: r.tags,
        })
    }
}

/// The transaction as it stands on `db`, legs in posting order.
pub async fn snapshot(db: &mut SqliteConnection, id: i64) -> Result<Snapshot> {
    let header: HeaderRow = sqlx::query_as(
        "SELECT date, payee, narration, reviewed != 0 AS reviewed FROM transactions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *db)
    .await?
    .with_context(|| format!("查無交易 {id}"))?;
    let legs: Vec<LegRow> = sqlx::query_as(
        "SELECT a.path, p.amount, p.currency, p.tags, p.origin FROM postings p
         JOIN accounts a ON a.id = p.account_id WHERE p.transaction_id = ? ORDER BY p.id",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    Ok(Snapshot {
        date: header.date.parse().with_context(|| format!("transaction {id} date"))?,
        payee: header.payee,
        narration: header.narration,
        reviewed: header.reviewed,
        legs: legs.into_iter().map(SnapshotLeg::try_from).collect::<Result<_>>()?,
    })
}

pub async fn record(
    db: &mut SqliteConnection,
    transaction_id: i64,
    kind: EventKind,
    payload: &impl Serialize,
) -> Result<()> {
    sqlx::query("INSERT INTO transaction_events (transaction_id, kind, payload) VALUES (?, ?, ?)")
        .bind(transaction_id)
        .bind(kind.to_string())
        .bind(serde_json::to_string(payload)?)
        .execute(db)
        .await
        .with_context(|| format!("recording {kind} on transaction {transaction_id}"))?;
    Ok(())
}

/// Every event on a transaction, oldest first.
pub async fn history(pool: &SqlitePool, transaction_id: i64) -> Result<Vec<Event>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT at, kind, payload FROM transaction_events WHERE transaction_id = ? ORDER BY id",
    )
    .bind(transaction_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(at, kind, payload)| {
            Ok(Event {
                at,
                kind: kind.parse().with_context(|| format!("event {kind:?}"))?,
                payload,
            })
        })
        .collect()
}
