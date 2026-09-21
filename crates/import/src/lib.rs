//! Importers: institution downloads into SQLite, deduplicated against what
//! the ledger already holds and gated by the balance check.

pub mod cathay_bank;
pub mod matcher;
pub mod plan;
