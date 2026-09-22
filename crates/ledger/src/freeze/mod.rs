//! The **freeze tool**: the one-time / audit-time step that
//! reconciles the full history and exports it as a trusted
//! [`journal`](super::journal).
//!
//! All the heavy machinery lives here — [`assemble`](super::build::assemble)'s
//! reconciliation and the balance checks — so the app's ongoing load path
//! (`db::load`) carries none of it. It is meant to be re-run while the
//! corrected records are still being audited; each run regenerates the
//! journal. The journal is trusted only after it verifies: freeze writes it
//! under a staged name, re-reads that, and proves the rows reproduce every
//! balance assertion and that no asset account closes negative (a split account
//! may: that means you owe them). Only then does it move the journal and its
//! assertions into place; a failed run discards the staged files and leaves the
//! last verified pair untouched.
//!
//! ```text
//! cargo run -- freeze --journal ledger/journal.csv \
//!   --cathay-statements raw/cathay-bank/*/*.csv \
//!   --line-bank-statements raw/line-bank/*/*.pdf \
//!   --transactions corrected/transactions.csv
//! ```

mod manual;
mod runner;

pub use runner::{run, FreezeArgs, Mismatch, Negative, Report};
