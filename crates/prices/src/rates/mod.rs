//! FX rates for the ledger: which days each currency needs a rate, which the
//! cache already has, and fetching the difference.
//!
//! One computation answers all three callers: the fetch (CLI and the server's
//! worker) asks the source for the gaps, `load-journal` names the currencies
//! with no rate at all, and the coverage page lists the rest.

mod coverage;
mod fetch;
mod route;

pub use coverage::{coverage, Coverage, Gap, Need};
pub use fetch::{fetch, status, Fetched, QuoteSource, RateStore};
pub use route::{rated, routes, tickers, Leg};
