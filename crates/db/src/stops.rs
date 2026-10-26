//! `import_stop`: why an import stopped, typed so the review queue can list
//! each until the cause is fixed and the import runs again. The queue has a
//! view for every [`Stop`], so a new way to stop needs one too.

mod stop;
mod store;

pub use stop::{Record, Refused, Stop};
pub use store::{clear, finish, open, record};
