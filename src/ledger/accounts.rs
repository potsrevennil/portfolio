//! The chart of accounts: 天天記帳 names → Beancount accounts.
//!
//! Beancount rejects non-ASCII account names, so every Chinese category and
//! account name needs an explicit ASCII counterpart. See `ledger/mapping.toml`.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    ops::Deref,
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result};
use serde::Deserialize;

/// One of Beancount's five account roots — the part of an account name that
/// fixes its sign convention and which statement it lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountType {
    Assets,
    Liabilities,
    Income,
    Expenses,
    Equity,
}

impl FromStr for AccountType {
    type Err = ();

    /// The five roots Beancount allows, and nothing else.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "Assets" => Self::Assets,
            "Liabilities" => Self::Liabilities,
            "Income" => Self::Income,
            "Expenses" => Self::Expenses,
            "Equity" => Self::Equity,
            _ => return Err(()),
        })
    }
}

/// A Beancount account name, e.g. `Assets:Bank:Savings`.
///
/// Its own type rather than a bare `String` so a field holding an account
/// cannot be mixed up with one holding an app-side name or a bank's wording. It
/// parses through `FromStr` (also how it deserializes): an empty value is
/// allowed and means "deliberately unmapped"; any non-empty value must be a
/// colon path whose first segment is one of the five roots, or it is rejected
/// with the name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Account(String);

/// `Deref` and `AsRef` let an `Account` stand in for a `str` — `is_empty`,
/// `split`, `to_string`, comparison — so it carries no accessor or `Display` of
/// its own.
impl Deref for Account {
    type Target = str;

    fn deref(&self) -> &str { &self.0 }
}

impl AsRef<str> for Account {
    fn as_ref(&self) -> &str { &self.0 }
}

impl FromStr for Account {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let name = s.trim();
        if name.is_empty() {
            return Ok(Account(String::new()));
        }
        if !name.is_ascii() {
            return Err(format!("{name:?} is not ASCII, which Beancount account names must be"));
        }
        if name.split(':').next().unwrap_or("").parse::<AccountType>().is_err() {
            return Err(format!(
                "{name:?} is not a Beancount account: the first segment must be one of Assets, \
                 Liabilities, Income, Expenses, Equity"
            ));
        }
        if name.split(':').any(str::is_empty) {
            return Err(format!("{name:?} has an empty account segment"));
        }
        Ok(Account(name.to_string()))
    }
}

impl TryFrom<String> for Account {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> { s.parse() }
}

/// An account plus the tags that carry the detail the shallow account tree no
/// longer encodes. Written in the config as a bare account string when it has
/// no tags, or as `{ account = "...", tags = [...] }` when it does — one shape
/// to the code either way.
#[derive(Debug, Default, Deserialize)]
#[serde(from = "Raw")]
pub struct Mapping {
    pub account: Account,
    pub tags: Vec<String>,
}

/// A mapping as written: a bare account, or an account with tags. Only a
/// parsing shape — the code sees the flattened `Mapping`, never this.
#[derive(Deserialize)]
#[serde(untagged)]
enum Raw {
    Bare(Account),
    Tagged {
        account: Account,
        #[serde(default)]
        tags: Vec<String>,
    },
}

impl From<Raw> for Mapping {
    fn from(raw: Raw) -> Self {
        match raw {
            Raw::Bare(account) => Mapping { account, tags: Vec::new() },
            Raw::Tagged { account, tags } => Mapping { account, tags },
        }
    }
}

/// A single record the category mapping gets wrong.
///
/// 天天記帳 offers one category per record, so an event the app can only file
/// as 投資 or 其他 may really be something the category has no word for. Keyed
/// by the record's UUID — the export's last column, which is stable across
/// re-exports — so the correction survives regeneration without editing
/// generated files.
#[derive(Debug, Deserialize)]
pub struct Override {
    /// Replaces whatever the category would have resolved to. Empty means the
    /// entry is deliberately inert, matching how a blank mapping is treated.
    pub account: Account,
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
/// here rather than compiled in. The rest name the accounts and app-side
/// buckets the build reaches by role — backfilled activity through `primary`,
/// settlement through `settlement` — since a role cannot be looked up by a name
/// the config is free to choose.
#[derive(Debug, Default, Deserialize)]
pub struct Institution {
    /// Statement account number → ledger account. The key is the bank's own
    /// identifier for the account, which is a string and nothing more; the
    /// value is where its lines are posted.
    #[serde(default)]
    pub accounts: HashMap<String, Account>,
    /// What the bookkeeping app calls all of them together, having no notion
    /// that one institution can hold several accounts. An app-side name, not a
    /// ledger account, so it stays a plain string.
    #[serde(default)]
    pub app_account: String,
    /// Where activity with no statement of its own is attributed.
    #[serde(default)]
    pub primary: Account,
    /// Where securities settlement passes through.
    #[serde(default)]
    pub settlement: Account,
    /// The app account whose movements are securities settlement rather than
    /// ordinary spending. Backfilled activity naming it is routed through
    /// `settlement`; everything else goes through `primary`. App-side name, so
    /// a plain string like `app_account`.
    #[serde(default)]
    pub settlement_app_account: String,
    /// Catches internal transfers whose two halves land on different days, so a
    /// non-zero balance means money was genuinely in transit at the period end.
    #[serde(default)]
    pub clearing: Account,
}

/// Where a statement line goes when no app record explains it.
#[derive(Debug, Default, Deserialize)]
pub struct Fallback {
    pub income: Account,
    pub expense: Account,
    /// Statement descriptions whose contra is known without an app record, such
    /// as interest the bank pays and fees it charges.
    #[serde(default)]
    pub descriptions: HashMap<String, Account>,
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
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let chart: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        chart.validate().with_context(|| format!("in {}", path.display()))?;
        Ok(chart)
    }

    /// Fails on a config the build would otherwise turn into a ledger full of
    /// empty account names, which Beancount rejects far from the actual cause.
    fn validate(&self) -> Result<()> {
        let required = [
            ("institution.app_account", self.institution.app_account.is_empty()),
            ("institution.primary", self.institution.primary.is_empty()),
            ("institution.settlement", self.institution.settlement.is_empty()),
            (
                "institution.settlement_app_account",
                self.institution.settlement_app_account.is_empty(),
            ),
            ("institution.clearing", self.institution.clearing.is_empty()),
            ("fallback.income", self.fallback.income.is_empty()),
            ("fallback.expense", self.fallback.expense.is_empty()),
        ];
        for (name, empty) in required {
            anyhow::ensure!(!empty, "{name} is required and must not be empty");
        }
        anyhow::ensure!(
            !self.institution.accounts.is_empty(),
            "institution.accounts must name at least one statement account number"
        );
        Ok(())
    }

    /// An empty account means "deliberately unmapped" and is treated as
    /// missing.
    fn get<'a>(table: &'a HashMap<String, Mapping>, key: &str) -> Option<&'a Mapping> {
        table.get(key).filter(|t| !t.account.is_empty())
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
