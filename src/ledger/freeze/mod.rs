//! The **freeze tool** (design doc T2): the one-time / audit-time step that
//! reconciles the full history and exports it as a trusted
//! [`seed`](super::seed).
//!
//! All the heavy machinery lives here — [`assemble`](super::build::assemble)'s
//! reconciliation and the balance checks — so the app's ongoing load path
//! ([`super::load`]) carries none of it. It is meant to be re-run while the
//! corrected 天天記帳 copies are still being audited; each run regenerates the
//! seed. The seed is trusted only after it verifies: freeze re-reads what it
//! wrote and proves the rows reproduce every balance assertion and that no
//! asset account closes negative, else it removes the seed and stops.
//!
//! ```text
//! cargo run -- freeze --seed ledger/seed.csv \
//!   --cathay-statements <活存.csv> <投資.csv> \
//!   --daily-income-expense <收支.csv> --daily-transfers <轉帳.csv> \
//!   --daily-backfill
//! ```

mod manual;
mod runner;

pub use runner::{run, FreezeArgs, Mismatch, Negative, Report};
