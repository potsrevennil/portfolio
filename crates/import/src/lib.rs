//! Importers: institution downloads into SQLite, deduplicated against what
//! the ledger already holds and gated by the balance check.

pub mod bank;
pub mod broker;
pub mod counted;
pub mod matcher;
pub mod pairing;
pub mod plan;
pub mod tiantian;
