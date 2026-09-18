//! Builds a Beancount ledger from downloaded statements.
//!
//! Statements say money moved but not where it went, so the other side of each
//! posting comes from 天天記帳 — either a spending category or the account it
//! went to. See `matching` for how the two are brought together.

pub mod accounts;
pub mod args;
pub mod build;
pub mod daily;
pub mod emit;
pub mod freeze;
pub mod journal;
pub mod load;
pub mod matching;
pub mod model;
pub mod names;
pub mod rates;
pub mod statements;
pub mod summary;
pub mod writer;

pub use args::Args;
pub use build::build;
