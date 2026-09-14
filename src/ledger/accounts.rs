//! The chart of accounts: 天天記帳 names → Beancount accounts.
//!
//! Beancount rejects non-ASCII account names, so every Chinese category and
//! account name needs an explicit ASCII counterpart. See `ledger/mapping.toml`.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Either a bare account name, or an account plus the tags that carry the detail
/// the account tree no longer encodes.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Mapping {
    Account(String),
    Tagged {
        account: String,
        #[serde(default)]
        tags: Vec<String>,
    },
}

impl Mapping {
    pub fn account(&self) -> &str {
        match self {
            Mapping::Account(a) => a,
            Mapping::Tagged { account, .. } => account,
        }
    }

    pub fn tags(&self) -> &[String] {
        match self {
            Mapping::Account(_) => &[],
            Mapping::Tagged { tags, .. } => tags,
        }
    }
}

/// A single record the category mapping gets wrong.
///
/// 天天記帳 offers one category per record, so an event the app can only file as
/// 投資 or 其他 may really be something the category has no word for. Keyed by the
/// record's UUID — the export's last column, which is stable across re-exports —
/// so the correction survives regeneration without editing generated files.
#[derive(Debug, Deserialize)]
pub struct Override {
    /// Replaces whatever the category would have resolved to.
    pub account: String,
    /// The app leaves 備註 empty on most records, so this is usually the only
    /// place the event gets described.
    #[serde(default)]
    pub narration: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The bank whose statements drive the ledger.
///
/// Account numbers are the customer's, not the importer's, so they are named
/// here rather than compiled in. Everything else in this struct is an account
/// the build has to reach by role — it routes backfilled activity through
/// `primary` and settlement through `settlement` — and a role cannot be looked
/// up by a name the config is free to choose.
#[derive(Debug, Default, Deserialize)]
pub struct Institution {
    /// Statement account number → ledger account.
    #[serde(default)]
    pub accounts: HashMap<String, String>,
    /// What the bookkeeping app calls all of them together, having no notion
    /// that one institution can hold several accounts.
    #[serde(default)]
    pub app_account: String,
    /// Where activity with no statement of its own is attributed.
    #[serde(default)]
    pub primary: String,
    /// Where securities settlement passes through.
    #[serde(default)]
    pub settlement: String,
    /// The app account whose movements are securities settlement rather than
    /// ordinary spending. Backfilled activity naming it is routed through
    /// `settlement`; everything else goes through `primary`.
    #[serde(default)]
    pub settlement_app_account: String,
    /// Catches internal transfers whose two halves land on different days, so a
    /// non-zero balance means money was genuinely in transit at the period end.
    #[serde(default)]
    pub clearing: String,
}

/// Where a statement line goes when no app record explains it.
#[derive(Debug, Default, Deserialize)]
pub struct Fallback {
    pub income: String,
    pub expense: String,
    /// Statement descriptions whose contra is known without an app record, such
    /// as interest the bank pays and fees it charges.
    #[serde(default)]
    pub descriptions: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Chart {
    #[serde(default)]
    pub expenses: HashMap<String, Mapping>,
    #[serde(default)]
    pub income: HashMap<String, Mapping>,
    #[serde(default)]
    pub accounts: HashMap<String, Mapping>,
    #[serde(default)]
    pub overrides: HashMap<String, Override>,
    #[serde(default)]
    pub institution: Institution,
    #[serde(default)]
    pub fallback: Fallback,
}

impl Chart {
    pub fn load(path: &str) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("reading {}", path))?;
        let chart: Self = toml::from_str(&text).with_context(|| format!("parsing {}", path))?;
        chart.validate().with_context(|| format!("in {}", path))?;
        Ok(chart)
    }

    /// Fails on a config the build would otherwise turn into a ledger full of
    /// empty account names, which Beancount rejects far from the actual cause.
    fn validate(&self) -> Result<()> {
        let required = [
            ("institution.app_account", &self.institution.app_account),
            ("institution.primary", &self.institution.primary),
            ("institution.settlement", &self.institution.settlement),
            ("institution.settlement_app_account", &self.institution.settlement_app_account),
            ("institution.clearing", &self.institution.clearing),
            ("fallback.income", &self.fallback.income),
            ("fallback.expense", &self.fallback.expense),
        ];
        for (name, value) in required {
            anyhow::ensure!(!value.is_empty(), "{name} is required and must not be empty");
        }
        anyhow::ensure!(
            !self.institution.accounts.is_empty(),
            "institution.accounts must name at least one statement account number"
        );
        Ok(())
    }

    /// An empty account means "deliberately unmapped" and is treated as missing.
    fn get<'a>(table: &'a HashMap<String, Mapping>, key: &str) -> Option<&'a Mapping> {
        table.get(key).filter(|t| !t.account().is_empty())
    }

    pub fn category(&self, name: &str, is_income: bool) -> Option<&Mapping> {
        Self::get(if is_income { &self.income } else { &self.expenses }, name)
    }

    pub fn account(&self, name: &str) -> Option<&Mapping> { Self::get(&self.accounts, name) }

    pub fn override_for(&self, id: &str) -> Option<&Override> {
        self.overrides.get(id).filter(|o| !o.account.is_empty())
    }

    /// Corrections that named a record the exports do not contain.
    pub fn stale_overrides(&self, used: &BTreeSet<String>) -> BTreeSet<String> {
        self.overrides
            .iter()
            .filter(|(id, o)| !o.account.is_empty() && !used.contains(*id))
            .map(|(id, _)| id.clone())
            .collect()
    }
}
