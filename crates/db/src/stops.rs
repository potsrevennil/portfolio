//! `import_stop`: why an import stopped, typed so the review queue can show
//! each and settle it before the import runs again. The queue has a view for
//! every [`Stop`], so a new way to stop needs one too.

mod settle;
mod stop;
mod store;

pub use settle::{choose, detach, move_leg};
pub use stop::{Pick, Record, Refused, Stop};
pub use store::{clear, finish, open, picks, record, Open};
