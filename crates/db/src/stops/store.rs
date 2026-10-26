//! Keeping stops across a rollback, and clearing them when a run succeeds.

use anyhow::{Context, Result};
use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

use super::{Refused, Stop};
use crate::pairing;

/// Records what a refused run of `source` met, in place of what its last run
/// left open.
pub async fn record(db: &mut SqliteConnection, source: &str, stops: &[Stop]) -> Result<()> {
    clear(db, source).await?;
    let mut lines = Vec::new();
    for stop in stops {
        if let Stop::LineAmbiguous(a) = stop {
            lines.push(a.clone());
            continue;
        }
        sqlx::query(
            "INSERT INTO import_stop (source, kind, subject, problem) VALUES (?, ?, ?, ?)
             ON CONFLICT (source, kind, subject) DO UPDATE SET problem = excluded.problem,
                cleared_at = NULL",
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
pub async fn open(pool: &SqlitePool) -> Result<Vec<Stop>> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT problem FROM import_stop WHERE cleared_at IS NULL ORDER BY id")
            .fetch_all(pool)
            .await
            .context("reading import stops")?;
    rows.iter().map(|r| serde_json::from_str(r).context("an import stop")).collect()
}
