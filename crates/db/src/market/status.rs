//! Each background job's last run, as the fetch recorded it: what the
//! coverage page shows beside statement freshness.

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::Market;

/// The rate fetch's job name in `fetch_status`.
pub const RATES_JOB: &str = "rates";

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LastRun {
    pub last_attempt: DateTime<Utc>,
    /// `None` until a run first succeeds.
    pub last_success: Option<DateTime<Utc>>,
    /// The last run's error; `None` once a run succeeds.
    pub last_error: Option<String>,
}

impl Market {
    /// `None` when `job` has never run.
    pub async fn last_run(&self, job: &str) -> Result<Option<LastRun>> {
        Ok(sqlx::query_as(
            "SELECT last_attempt, last_success, last_error FROM fetch_status WHERE job = ?",
        )
        .bind(job)
        .fetch_optional(self.pool())
        .await?)
    }
}
