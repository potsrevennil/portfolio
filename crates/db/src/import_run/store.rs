//! Keeping what a run touched, and reading it back for the review page.

use std::{collections::BTreeMap, fmt};

use anyhow::{Context, Result};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use super::Suggestion;
use crate::review;

/// The transactions one import run touched.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Run(BTreeMap<i64, Vec<Suggestion>>);

impl Run {
    pub fn touched(&mut self, transaction_id: i64) { self.0.entry(transaction_id).or_default(); }

    pub fn suggest(&mut self, transaction_id: i64, suggestion: Suggestion) {
        let suggestions = self.0.entry(transaction_id).or_default();
        if !suggestions.contains(&suggestion) {
            suggestions.push(suggestion);
        }
    }

    pub fn len(&self) -> usize { self.0.len() }

    pub fn is_empty(&self) -> bool { self.0.is_empty() }
}

impl fmt::Display for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (id, suggestions) in self.0.iter().filter(|(_, s)| !s.is_empty()) {
            for s in suggestions {
                writeln!(f, "  transaction {id}: {s}")?;
            }
        }
        Ok(())
    }
}

/// Keeps `run`, and confirms each row it suggests nothing for. A run that
/// touched nothing leaves the last one standing.
pub async fn record(db: &mut SqliteConnection, source: &str, run: &Run) -> Result<()> {
    if run.is_empty() {
        return Ok(());
    }
    let run_id: i64 = sqlx::query_scalar("INSERT INTO import_run (source) VALUES (?) RETURNING id")
        .bind(source)
        .fetch_one(&mut *db)
        .await
        .context("recording an import run")?;
    for (id, suggestions) in &run.0 {
        sqlx::query(
            "INSERT INTO import_item (run_id, transaction_id, suggestions) VALUES (?, ?, ?)",
        )
        .bind(run_id)
        .bind(id)
        .bind(serde_json::to_string(suggestions)?)
        .execute(&mut *db)
        .await
        .with_context(|| format!("recording transaction {id} of an import run"))?;
        if suggestions.is_empty() {
            review::confirm_in(db, *id).await?;
        }
    }
    Ok(())
}

/// What the newest run to touch each of `ids` suggests for it.
pub async fn suggestions(pool: &SqlitePool, ids: &[i64]) -> Result<BTreeMap<i64, Vec<Suggestion>>> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT transaction_id, suggestions FROM import_item i
         WHERE run_id = (SELECT MAX(run_id) FROM import_item
                         WHERE transaction_id = i.transaction_id)
           AND transaction_id IN (",
    );
    let mut list = q.separated(", ");
    for id in ids {
        list.push_bind(id);
    }
    q.push(")");
    let rows: Vec<(i64, String)> =
        q.build_query_as().fetch_all(pool).await.context("reading import suggestions")?;
    rows.into_iter()
        .map(|(id, s)| Ok((id, serde_json::from_str(&s).context("import suggestions")?)))
        .collect()
}

/// The confirmed rows of each importer's newest run, newest first.
pub async fn confirmed(pool: &SqlitePool) -> Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT t.id FROM import_item i
         JOIN transactions t ON t.id = i.transaction_id
         WHERE i.run_id IN (SELECT MAX(id) FROM import_run GROUP BY source)
           AND t.reviewed != 0 AND t.deleted_at IS NULL
         ORDER BY t.date DESC, t.id DESC",
    )
    .fetch_all(pool)
    .await
    .context("reading what the last imports confirmed")
}
