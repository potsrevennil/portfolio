//! Importers: institution downloads into SQLite, deduplicated against what
//! the ledger already holds; the balance check reports what does not agree.

pub mod bank;
pub mod broker;
pub mod counted;
pub mod matcher;
pub mod pairing;
pub mod plan;
pub mod tiantian;
