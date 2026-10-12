//! All SQLite access: the schema, its migrations and every query. Other crates
//! hold no SQL; where their logic needs storage they declare a trait
//! (`prices::PriceStore`, `portfolio::SplitStore`) and this crate implements
//! it.

pub mod account_status;
pub mod assertions;
pub mod broker;
pub mod chart;
pub mod check;
pub mod connect;
pub mod events;
pub mod hledger;
pub mod holdings;
pub mod import;
pub mod import_batch;
pub mod journal;
pub mod load;
pub mod pairing;
pub mod query;
pub mod quotes;
pub mod review;
pub mod splits;

pub use connect::{connect, init_db, open_db};
// The handles callers pass around, so only this crate depends on sqlx.
pub use sqlx::{SqliteConnection, SqlitePool};
