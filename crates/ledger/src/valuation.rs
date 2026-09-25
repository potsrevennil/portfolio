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

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

/// A misspelled key would otherwise leave the list empty and count the
/// holding in net worth.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// The file is shared with Fava, so other sections are allowed; one whose
    /// name only nearly reads `at_cost` is refused.
    pub fn parse(text: &str) -> Result<Self> {
        let table: toml::Table = toml::from_str(text)?;
        let squash = |k: &str| k.to_lowercase().replace(['_', '-', ' '], "");
        match table.keys().find(|k| *k != "at_cost" && squash(k) == "atcost") {
            Some(near) => {
                anyhow::bail!("[{near}] is not a section; holdings at cost go under [at_cost]")
            }
            None => {
                let config: Config = toml::Value::Table(table).try_into()?;
                Ok(Self { roots: config.at_cost.accounts })
            }
        }
    }

    /// True for a listed account and everything under it.
    pub fn covers(&self, account: &str) -> bool { self.holding(account).is_some() }

    /// The holding `account` belongs to: a listed account holds itself, and
    /// an account under one belongs to the listed account's child it sits
    /// in (`Assets:Unlisted:Alpha:Shares` → `Assets:Unlisted:Alpha`).
    pub fn holding<'a>(&self, account: &'a str) -> Option<&'a str> {
        self.roots.iter().find_map(|root| {
            let rest = account.strip_prefix(root.as_str())?;
            match rest.strip_prefix(':') {
                None if rest.is_empty() => Some(account),
                None => None,
                Some(below) => {
                    let child = below.split(':').next().unwrap_or(below);
                    Some(&account[..root.len() + 1 + child.len()])
                }
            }
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
    fn a_holding_is_the_listed_account_or_its_child() {
        let at_cost = AtCost::parse("[at_cost]\naccounts = [\"Assets:Unlisted\"]\n").unwrap();
        assert_eq!(at_cost.holding("Assets:Unlisted"), Some("Assets:Unlisted"));
        assert_eq!(at_cost.holding("Assets:Unlisted:Alpha"), Some("Assets:Unlisted:Alpha"));
        assert_eq!(at_cost.holding("Assets:Unlisted:Alpha:Shares"), Some("Assets:Unlisted:Alpha"));
        assert_eq!(at_cost.holding("Assets:Unlisted-Other"), None);
        assert_eq!(at_cost.holding("Assets:Cash"), None);
    }

    #[test]
    fn a_misspelled_key_or_section_is_refused() {
        assert!(AtCost::parse("[at_cost]\naccount = [\"Assets:Unlisted\"]\n").is_err());
        assert!(AtCost::parse("[at-cost]\naccounts = [\"Assets:Unlisted\"]\n").is_err());
        assert!(AtCost::parse("[AtCost]\naccounts = [\"Assets:Unlisted\"]\n").is_err());
        // Fava's own sections stay allowed.
        assert!(AtCost::parse("[credit_limits]\n\"卡\" = 1\n").is_ok());
    }

    #[test]
    fn a_config_without_the_section_covers_nothing() {
        assert!(!AtCost::parse("[display]\n\"Assets\" = \"資產\"\n").unwrap().covers("Assets"));
    }
}
