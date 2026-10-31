//! Bill splitting (spec §5): people, splits, and the shares that tie postings
//! on split accounts to a split. Postings already carry the balances, so a
//! split only groups them: every charge, share, repayment and write-off stays
//! its own record, and grouping history is optional. `crate::splits` is stock
//! splits.

mod anchor;
mod books;
mod detach;
mod linking;
mod mix;
mod model;
mod people;
mod read;
mod store;
mod views;

pub use anchor::{anchor_check, Anchor, AnchorRow, Anchored};
pub use books::{Books, BAD_DEBT};
pub use detach::{delete, detach};
pub use linking::Repayment;
pub(crate) use mix::mixed;
pub use mix::Mixed;
pub use model::{Closed, Closing, Person, Role, Shape, Share, Split, Status};
pub use people::{add_person, assign, people, rename_person, unassign};
pub(crate) use read::held;
pub use store::{attach, create, gather, rename, write_off, Attach, WriteOff};
pub use views::{
    attachable, closed, get, list, open_for, unallocated, Candidate, ClosedFilter, OpenShare,
    Unallocated,
};
