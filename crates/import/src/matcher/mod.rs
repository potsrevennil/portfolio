//! Matcher: places incoming records against a pool of candidates, one
//! [`MatchMode`] at a time. Import dedup is separate, in
//! `db::import`.

pub mod engine;
pub mod split_repayment;
pub mod statement_line;

pub use engine::{
    Available, CommitError, Consumption, Engine, Match, MatchMode, Outcome, Pool, Record,
};
pub use split_repayment::SplitRepaymentMode;
pub use statement_line::StatementLineMode;
