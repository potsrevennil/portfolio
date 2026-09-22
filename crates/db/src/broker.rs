//! The `broker_record` table: the broker sub-ledger, one row per statement
//! line, deduplicated on `external_ref`.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};
use portfolio::broker::BrokerRecord;
use sqlx::SqliteConnection;

/// A record ready to store: which ledger account it belongs to and its
/// namespaced ref.
#[derive(Clone, Debug)]
pub struct NewRecord {
    pub account: String,
    pub external_ref: String,
    pub import_batch_id: Option<i64>,
    pub record: BrokerRecord,
}

/// Stores `r`; the caller has already checked the ref is new.
pub async fn insert(conn: &mut SqliteConnection, r: &NewRecord) -> Result<i64> {
    let account_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE path = ?")
        .bind(&r.account)
        .fetch_optional(&mut *conn)
        .await?
        .with_context(|| format!("broker record names an unknown account {}", r.account))?;
    let b = &r.record;
    Ok(sqlx::query(
        "INSERT INTO broker_record (account_id, external_ref, kind, trade_date, settle_date,
             executed_at, symbol, quantity, price, amount, commission, currency, description,
             import_batch_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(account_id)
    .bind(&r.external_ref)
    .bind(b.kind.to_string())
    .bind(b.trade_date.map(|d| d.to_string()))
    .bind(b.settle_date.to_string())
    .bind(b.executed_at.map(|t| t.to_rfc3339()))
    .bind(&b.symbol)
    .bind(b.quantity.to_string())
    .bind(b.price.to_string())
    .bind(b.amount.to_string())
    .bind(b.commission.to_string())
    .bind(b.currency.to_string())
    .bind(&b.description)
    .bind(r.import_batch_id)
    .execute(conn)
    .await
    .with_context(|| format!("inserting broker record {}", r.external_ref))?
    .last_insert_rowid())
}

/// Every stored ref starting with `prefix`.
pub async fn refs(conn: &mut SqliteConnection, prefix: &str) -> Result<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT external_ref FROM broker_record WHERE substr(external_ref, 1, length(?1)) = ?1",
    )
    .bind(prefix)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().collect())
}

#[derive(sqlx::FromRow)]
struct Row {
    account: String,
    external_ref: String,
    kind: String,
    trade_date: Option<String>,
    settle_date: String,
    executed_at: Option<String>,
    symbol: Option<String>,
    quantity: String,
    price: String,
    amount: String,
    commission: String,
    currency: String,
    description: String,
}

impl TryFrom<Row> for (String, BrokerRecord) {
    type Error = anyhow::Error;

    fn try_from(r: Row) -> Result<Self> {
        let parsed = || -> Result<BrokerRecord> {
            Ok(BrokerRecord {
                kind: r.kind.parse()?,
                trade_date: r.trade_date.as_deref().map(str::parse).transpose()?,
                settle_date: r.settle_date.parse()?,
                executed_at: r.executed_at.as_deref().map(str::parse).transpose()?,
                symbol: r.symbol.clone(),
                quantity: r.quantity.parse()?,
                price: r.price.parse()?,
                amount: r.amount.parse()?,
                commission: r.commission.parse()?,
                currency: r.currency.parse()?,
                description: r.description.clone(),
                key: r.external_ref.clone(),
            })
        };
        let record =
            parsed().with_context(|| format!("invalid broker_record {}", r.external_ref))?;
        Ok((r.account, record))
    }
}

/// Every record by account path, oldest first. `key` holds the stored ref.
pub async fn load(conn: &mut SqliteConnection) -> Result<BTreeMap<String, Vec<BrokerRecord>>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.path AS account, r.external_ref, r.kind, r.trade_date, r.settle_date,
                r.executed_at, r.symbol, r.quantity, r.price, r.amount, r.commission,
                r.currency, r.description
         FROM broker_record r JOIN accounts a ON a.id = r.account_id
         ORDER BY a.path, r.settle_date, r.id",
    )
    .fetch_all(conn)
    .await
    .context("loading broker records")?;
    let mut out: BTreeMap<String, Vec<BrokerRecord>> = BTreeMap::new();
    for row in rows {
        let (account, record) = row.try_into()?;
        out.entry(account).or_default().push(record);
    }
    Ok(out)
}
