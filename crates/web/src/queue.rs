//! Builds the review queue from `db`.

use anyhow::Result;
use db::{
    journal::{self, Filter},
    SqlitePool,
};
use ledger_types::currency::Currency;

use crate::{
    entries,
    model::{JournalQuery, Queue, Review},
};

/// How many transactions wait for review.
pub async fn count(pool: &SqlitePool) -> Result<u64> {
    journal::count(pool, &Filter { reviewed: Some(false), ..Default::default() }).await
}

/// The journal held to unreviewed records, whatever the query asks.
pub async fn load(pool: &SqlitePool, query: &JournalQuery, base: Currency) -> Result<Queue> {
    let journal = entries::load(pool, &query.with_review(Review::Unreviewed), base).await?;
    Ok(Queue { journal })
}
