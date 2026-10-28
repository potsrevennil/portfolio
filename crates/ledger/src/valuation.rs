//! Accounts whose recorded balance is what was paid, not what it is worth.
//!
//! An unlisted holding has no market price, so its balance is a cost, and
//! adding it to a net-worth total would read as a valuation nobody made. Such
//! accounts are listed in `mapping.toml`:
//!
//! ```toml
//! [at_cost]
//! accounts = ["Assets:Unlisted"]
//! ```
//!
//! Config rather than a column on the account: which holdings have a real
//! valuation changes with the data, not with the ledger's history.

use std::path::Path;

use anyhow::Result;

use crate::roots::Roots;

/// The configured subtrees. Empty means every balance is a valuation.
#[derive(Debug, Default, Clone)]
pub struct AtCost(Roots);

impl AtCost {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> { Roots::load(path, "at_cost").map(Self) }

    pub fn parse(text: &str) -> Result<Self> { Roots::parse(text, "at_cost").map(Self) }

    /// True for a listed account and everything under it.
    pub fn covers(&self, account: &str) -> bool { self.0.covers(account) }

    /// The holding `account` belongs to: a listed account holds itself, and
    /// an account under one belongs to the listed account's child it sits
    /// in (`Assets:Unlisted:Alpha:Shares` → `Assets:Unlisted:Alpha`).
    pub fn holding<'a>(&self, account: &'a str) -> Option<&'a str> {
        let root = self.0.root_of(account)?;
        match account[root.len()..].strip_prefix(':') {
            None => Some(account),
            Some(below) => {
                let child = below.split(':').next().unwrap_or(below);
                Some(&account[..root.len() + 1 + child.len()])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holding_is_the_listed_account_or_its_child() {
        let at_cost = AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted\"]\n").unwrap();
        assert_eq!(at_cost.holding("Assets:Unlisted"), Some("Assets:Unlisted"));
        assert_eq!(at_cost.holding("Assets:Unlisted:Alpha"), Some("Assets:Unlisted:Alpha"));
        assert_eq!(at_cost.holding("Assets:Unlisted:Alpha:Shares"), Some("Assets:Unlisted:Alpha"));
        assert_eq!(at_cost.holding("Assets:Unlisted-Other"), None);
        assert_eq!(at_cost.holding("Assets:Cash"), None);
    }
}
