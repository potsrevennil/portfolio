//! market.db: quotes, FX rates and splits. A cache any run can fetch again,
//! so it lives apart from the ledger database, which keeps only records that
//! cannot be. Reporting reads it through a read-only pool; only the fetcher
//! writes it.

mod cache;
mod fetch;
mod status;
mod store;

pub use cache::{Market, BASE, DEFAULT_URL};
pub use fetch::{run, Args, Report, Unrated};
pub use status::{LastRun, RATES_JOB};
pub use store::Rates;
