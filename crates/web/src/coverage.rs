//! 對帳進度: one bar per account, showing how far its bank's own statements
//! (or a count) have checked it and how far its records run past that, so a
//! current-looking balance never passes for a checked one; and which
//! statements are due, by each account's own statement day.

mod actions;
mod bar;
#[cfg(feature = "ssr")]
pub mod build;
mod forms;
mod model;
mod page;

pub use actions::{confirm_balance, coverage_due, load_coverage, save_schedule, CoverageActions};
pub use model::{AccountRow, CadenceName, Coverage, Mark};
pub use page::{CoverageCount, CoveragePage, CoverageView};
