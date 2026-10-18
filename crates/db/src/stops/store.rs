//! Keeping stops across a rollback, and clearing them when a run succeeds.

use std::collections::HashMap;

use anyhow::{Context, Result};
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use super::{Pick, Refused, Stop};
use crate::pairing;

/// A stop no later run of its importer has cleared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Open {
    pub id: i64,
    pub stop: Stop,
    pub pick: Option<Pick>,
    /// Settled by a write; the next import is what is left.
    pub resolved: bool,
}

#[derive(sqlx::FromRow)]
struct OpenRow {
    id: i64,
    problem: String,
    pick: Option<String>,
    resolved: bool,
}

impl TryFrom<OpenRow> for Open {
    type Error = anyhow::Error;

    fn try_from(r: OpenRow) -> Result<Self> {
        Ok(Open {
            id: r.id,
            stop: serde_json::from_str(&r.problem).context("an import stop")?,
            pick: r.pick.map(|p| serde_json::from_str(&p)).transpose().context("a pick")?,
            resolved: r.resolved,
        })
    }
}

const OPEN: &str = "SELECT id, problem, pick, resolved_at IS NOT NULL AS resolved FROM import_stop
    WHERE cleared_at IS NULL";

/// Records what a refused run of `source` met, in place of what its last run
/// left open. A pick stays unless its own stop is met again, when it no
/// longer fits and a person picks again.
pub async fn record(db: &mut SqliteConnection, source: &str, stops: &[Stop]) -> Result<()> {
    sqlx::query(
        "UPDATE import_stop SET cleared_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE source = ? AND cleared_at IS NULL AND pick IS NULL",
    )
    .bind(source)
    .execute(&mut *db)
    .await?;
    let mut lines = Vec::new();
    for stop in stops {
        if let Stop::LineAmbiguous(a) = stop {
            lines.push(a.clone());
            continue;
        }
        sqlx::query(
            "INSERT INTO import_stop (source, kind, subject, problem) VALUES (?, ?, ?, ?)
             ON CONFLICT (kind, subject) DO UPDATE SET source = excluded.source,
                problem = excluded.problem, pick = NULL, resolved_at = NULL, cleared_at = NULL",
        )
        .bind(source)
        .bind(<&str>::from(stop))
        .bind(stop.subject())
        .bind(serde_json::to_string(stop)?)
        .execute(&mut *db)
        .await
        .with_context(|| format!("recording the stop {stop}"))?;
    }
    pairing::record(db, &lines).await
}

/// Ends an import run in `tx`: one that succeeded commits, clearing what
/// the last refusal left open; one that stopped rolls back, and what stopped
/// it is kept for the queue.
pub async fn finish<T>(
    pool: &SqlitePool,
    source: &str,
    mut tx: Transaction<'static, Sqlite>,
    outcome: Result<T>,
) -> Result<T> {
    match outcome {
        Ok(report) => {
            clear(&mut tx, source).await?;
            tx.commit().await?;
            Ok(report)
        }
        Err(e) => {
            tx.rollback().await?;
            if let Some(Refused(found)) = e.downcast_ref() {
                let mut kept = pool.begin().await?;
                record(&mut kept, source, found).await?;
                kept.commit().await?;
            }
            Err(e)
        }
    }
}

/// Clears what `source` left open: a run that succeeds met none of it.
pub async fn clear(db: &mut SqliteConnection, source: &str) -> Result<()> {
    sqlx::query(
        "UPDATE import_stop SET cleared_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE source = ? AND cleared_at IS NULL",
    )
    .bind(source)
    .execute(db)
    .await?;
    Ok(())
}

/// Every stop still open, oldest first.
pub async fn open(pool: &SqlitePool) -> Result<Vec<Open>> {
    let rows: Vec<OpenRow> = sqlx::query_as(&format!("{OPEN} ORDER BY id"))
        .fetch_all(pool)
        .await
        .context("reading import stops")?;
    rows.into_iter().map(Open::try_from).collect()
}

/// The picks `source`'s next run places its records by, by record ref.
pub async fn picks(db: &mut SqliteConnection, source: &str) -> Result<HashMap<String, Pick>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT subject, pick FROM import_stop
         WHERE source = ? AND cleared_at IS NULL AND pick IS NOT NULL",
    )
    .bind(source)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|(subject, pick)| Ok((subject, serde_json::from_str(&pick).context("a pick")?)))
        .collect()
}

pub(super) async fn open_one(db: &mut SqliteConnection, id: i64) -> Result<Open> {
    let row: OpenRow = sqlx::query_as(&format!("{OPEN} AND id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await?
        .with_context(|| format!("揣無這个匯入問題 {id}"))?;
    Open::try_from(row)
}

pub(super) async fn resolve(db: &mut SqliteConnection, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE import_stop SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}
