//! Chinese display labels for chart paths, from `mapping.toml`.
//!
//! The rule the Fava extension applies (`ledger/mandarin_overview.py`
//! `_label`), so the app and Fava name every account alike:
//!
//! 1. A `[display]` entry wins.
//! 2. Else the one category (`[expenses]`/`[income]`/`[accounts]`) mapped to
//!    the account names it; if several are, the one untagged among them does.
//! 3. Else the ASCII leaf.
//!
//! This is the label a tree shows. Outside a tree, a label two accounts share
//! needs its parent's in front; see
//! [`standalone_labels`](crate::store::chart::standalone_labels).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::Path,
};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::accounts::Mapping;

#[derive(Debug, Default, Deserialize)]
struct Sections {
    #[serde(default)]
    expenses: HashMap<String, Mapping>,
    #[serde(default)]
    income: HashMap<String, Mapping>,
    #[serde(default)]
    accounts: HashMap<String, Mapping>,
    #[serde(default)]
    display: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
pub struct Labels {
    explicit: BTreeMap<String, String>,
    /// Every category mapped to an account.
    derived: BTreeMap<String, BTreeSet<String>>,
    /// The untagged ones among them.
    plain: BTreeMap<String, BTreeSet<String>>,
}

impl Labels {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let sections: Sections = toml::from_str(text)?;
        let mut labels = Self { explicit: sections.display, ..Self::default() };
        let categories = [sections.expenses, sections.income, sections.accounts];
        for (name, mapping) in categories.iter().flatten() {
            let account = mapping.account.to_string();
            if account.is_empty() {
                continue;
            }
            labels.derived.entry(account.clone()).or_default().insert(name.clone());
            if mapping.tags.is_empty() {
                labels.plain.entry(account).or_default().insert(name.clone());
            }
        }
        Ok(labels)
    }

    pub fn label(&self, account: &str) -> String {
        let single = |names: Option<&BTreeSet<String>>| match names {
            Some(names) if names.len() == 1 => names.first().cloned(),
            _ => None,
        };
        self.explicit
            .get(account)
            .cloned()
            .or_else(|| single(self.derived.get(account)))
            .or_else(|| single(self.plain.get(account)))
            .unwrap_or_else(|| leaf(account).to_string())
    }
}

fn leaf(account: &str) -> &str { account.rsplit(':').next().unwrap_or(account) }

#[cfg(test)]
mod tests {
    use super::*;

    const MAPPING: &str = r#"
[expenses]
"交通" = "Expenses:Transport"
"停車" = { account = "Expenses:Transport", tags = ["parking"] }
"咖啡" = { account = "Expenses:Food", tags = ["coffee"] }
"飲食" = { account = "Expenses:Food", tags = ["dining"] }
"未用" = ""

[accounts]
"現金" = "Assets:Cash"
"甲分帳" = "Assets:Split:Alpha"

[display]
"Assets"               = "資產"
"Assets:Split:Alpha"   = "甲公司"
"Expenses:Food"        = "食食"
"#;

    #[test]
    fn follows_the_fava_rule() {
        let labels = Labels::parse(MAPPING).unwrap();
        // [display] beats the category.
        assert_eq!(labels.label("Assets:Split:Alpha"), "甲公司");
        assert_eq!(labels.label("Expenses:Food"), "食食");
        // One category names it; of several, the untagged one does.
        assert_eq!(labels.label("Assets:Cash"), "現金");
        assert_eq!(labels.label("Expenses:Transport"), "交通");
        // No label at all: the leaf.
        assert_eq!(labels.label("Assets:Broker"), "Broker");
    }

    #[test]
    fn the_example_mapping_parses() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/ledger/mapping.example.toml");
        let labels = Labels::load(path).unwrap();
        assert_eq!(labels.label("Assets"), "資產");
    }
}
