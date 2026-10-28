//! Accounts whose flow is an investment's result, not day-to-day income or
//! spending: a realised gain or loss, a dividend. Charts show their net result
//! apart, so a loss does not read as spending. Listed in `mapping.toml`:
//!
//! ```toml
//! [investments]
//! accounts = ["Income:Investment", "Expenses:Investment"]
//! ```

use std::path::Path;

use anyhow::Result;

use crate::roots::Roots;

/// The configured subtrees. Empty means every flow is day-to-day.
#[derive(Debug, Default, Clone)]
pub struct Investments(Roots);

impl Investments {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Roots::load(path, "investments").map(Self)
    }

    pub fn parse(text: &str) -> Result<Self> { Roots::parse(text, "investments").map(Self) }

    /// True for a listed account and everything under it.
    pub fn covers(&self, account: &str) -> bool { self.0.covers(account) }
}
