//! One fetch at a time across processes: the server's worker and a `rates`
//! run from the command line take the same lease, kept in the store.

use std::time::Duration;

use anyhow::Result;
use chrono::Utc;

use super::fetch::RateStore;

/// How long a lease lasts: far longer than a run, which takes seconds, and
/// what frees it if its holder crashed.
const TERM: Duration = Duration::from_secs(10 * 60);

/// Another process holds the lease; its run covers the same gaps.
#[derive(Debug, thiserror::Error)]
#[error("another rate fetch is running; try again when it finishes")]
pub struct Busy;

pub(super) struct Lease<'a, S> {
    store: &'a S,
    holder: String,
}

impl<'a, S: RateStore> Lease<'a, S> {
    /// The lease, or `None` while another holder's is live.
    pub(super) async fn take(store: &'a S) -> Result<Option<Self>> {
        let now = Utc::now();
        let holder = format!("{}:{}", std::process::id(), now.timestamp_micros());
        match store.lease(&holder, now, now + TERM).await? {
            true => Ok(Some(Self { store, holder })),
            false => Ok(None),
        }
    }

    pub(super) async fn release(self) -> Result<()> { self.store.release(&self.holder).await }
}
