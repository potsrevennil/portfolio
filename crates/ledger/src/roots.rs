//! A `mapping.toml` section listing account subtrees by their root, as
//! `[at_cost]` and `[investments]` do:
//!
//! ```toml
//! [at_cost]
//! accounts = ["Assets:Unlisted"]
//! ```

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::accounts::in_subtree;

/// A misspelled key would otherwise leave the list empty and silently cover
/// nothing.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    #[serde(default)]
    accounts: Vec<String>,
}

/// The roots one section lists. Empty when the section is absent.
#[derive(Debug, Default, Clone)]
pub struct Roots {
    roots: Vec<String>,
}

impl Roots {
    pub fn load(path: impl AsRef<Path>, section: &str) -> Result<Self> {
        let path = path.as_ref();
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, section).with_context(|| format!("parsing {}", path.display()))
    }

    /// The file is shared with Fava, so other sections are allowed; one whose
    /// name only nearly reads `section` is refused.
    pub fn parse(text: &str, section: &str) -> Result<Self> {
        let mut table: toml::Table = toml::from_str(text)?;
        let squash = |k: &str| k.to_lowercase().replace(['_', '-', ' '], "");
        match table.keys().find(|k| *k != section && squash(k) == squash(section)) {
            Some(near) => anyhow::bail!("[{near}] is not a section; use [{section}]"),
            None => {
                let listed: Section = match table.remove(section) {
                    Some(value) => value.try_into()?,
                    None => Section::default(),
                };
                Ok(Self { roots: listed.accounts })
            }
        }
    }

    /// True for a listed account and everything under it.
    pub fn covers(&self, account: &str) -> bool { self.root_of(account).is_some() }

    /// The listed root `account` is under, or is.
    pub fn root_of(&self, account: &str) -> Option<&str> {
        self.roots.iter().map(String::as_str).find(|root| in_subtree(account, root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_listed_subtrees_only() {
        let roots = Roots::parse("[s]\naccounts = [\"Assets:Unlisted\"]\n", "s").unwrap();
        assert!(roots.covers("Assets:Unlisted"));
        assert!(roots.covers("Assets:Unlisted:Alpha"));
        assert!(!roots.covers("Assets:Unlisted-Other"));
        assert!(!roots.covers("Assets:Cash"));
        assert!(!Roots::default().covers("Assets:Unlisted"));
    }

    #[test]
    fn a_misspelled_key_or_section_is_refused() {
        assert!(Roots::parse("[at_cost]\naccount = [\"A\"]\n", "at_cost").is_err());
        assert!(Roots::parse("[at-cost]\naccounts = [\"A\"]\n", "at_cost").is_err());
        assert!(Roots::parse("[AtCost]\naccounts = [\"A\"]\n", "at_cost").is_err());
        // Fava's own sections stay allowed.
        assert!(Roots::parse("[credit_limits]\n\"卡\" = 1\n", "at_cost").is_ok());
    }

    #[test]
    fn a_config_without_the_section_covers_nothing() {
        let roots = Roots::parse("[display]\n\"Assets\" = \"資產\"\n", "at_cost").unwrap();
        assert!(!roots.covers("Assets"));
    }
}
