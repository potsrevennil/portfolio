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
//! Reports show them at cost, apart from the totals. Deliberately read from
//! the config rather than stored per account: which holdings have a real
//! valuation changes with the data, not with the ledger's history.

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct Section {
    #[serde(default)]
    accounts: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    at_cost: Section,
}

/// The configured subtrees. Empty means every balance is a valuation.
#[derive(Debug, Default, Clone)]
pub struct AtCost {
    roots: Vec<String>,
}

impl AtCost {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        Ok(Self { roots: config.at_cost.accounts })
    }

    /// True for a listed account and everything under it.
    pub fn covers(&self, account: &str) -> bool {
        self.roots.iter().any(|root| {
            account.strip_prefix(root.as_str()).is_some_and(|r| r.is_empty() || r.starts_with(':'))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_listed_subtrees_only() {
        let at_cost = AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted\"]\n").unwrap();
        assert!(at_cost.covers("Assets:Unlisted"));
        assert!(at_cost.covers("Assets:Unlisted:Alpha"));
        assert!(!at_cost.covers("Assets:Unlisted-Other"));
        assert!(!at_cost.covers("Assets:Cash"));
        assert!(!AtCost::default().covers("Assets:Unlisted"));
    }

    #[test]
    fn a_config_without_the_section_covers_nothing() {
        assert!(!AtCost::parse("[display]\n\"Assets\" = \"資產\"\n").unwrap().covers("Assets"));
    }
}
