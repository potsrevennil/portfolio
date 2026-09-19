//! The chart of accounts: 天天記帳 names → Beancount accounts.
//!
//! Beancount rejects non-ASCII account names, so every Chinese category and
//! account name needs an explicit ASCII counterpart. See `ledger/mapping.toml`.

use std::{
    collections::{BTreeSet, HashMap},
    fmt, fs,
    ops::Deref,
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
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
/// longer encodes. Bare is just the empty-tags case.
#[derive(Debug, Default)]
pub struct Mapping {
    pub account: Account,
    pub tags: Vec<String>,
}

impl<'de> Deserialize<'de> for Mapping {
    /// The config writes a mapping either as a bare account string or as a
    /// `{ account, tags }` table. There is one `Mapping`; the two arms here are
    /// the two TOML shapes serde dispatches on, not two kinds of mapping.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct MappingVisitor;

        impl<'de> serde::de::Visitor<'de> for MappingVisitor {
            type Value = Mapping;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an account name or a { account, tags } table")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Mapping, E> {
                Ok(Mapping { account: s.parse().map_err(E::custom)?, tags: Vec::new() })
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Mapping, A::Error> {
                #[derive(Deserialize)]
                struct Fields {
                    account: Account,
                    #[serde(default)]
                    tags: Vec<String>,
                }
                let f = Fields::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(Mapping { account: f.account, tags: f.tags })
            }
        }

        deserializer.deserialize_any(MappingVisitor)
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

/// Split accounts: running two-way balances with a person or group — a family
/// tab, a Splitwise group. Accountants call these current accounts: one account
/// per person or group, and the sign says who owes whom (positive, they owe
/// you; negative, you owe them). Negative is therefore an ordinary state here,
/// a payable, not a sign of a missing opening balance.
#[derive(Debug, Default, Deserialize)]
pub struct SplitAccounts {
    /// Every account at or under one of these is a split account.
    #[serde(default)]
    pub roots: Vec<Account>,
}

/// Rejects the section's old name. `Chart` ignores unknown sections, so a
/// leftover `[counterparty]` would otherwise be dropped silently: the
/// exemption would switch off and the freeze would fail on a negative split
/// account with no hint why.
fn renamed_to_split_accounts<'de, D: serde::Deserializer<'de>>(
    _: D,
) -> std::result::Result<(), D::Error> {
    Err(serde::de::Error::custom("[counterparty] was renamed to [split_accounts]"))
}

/// Rejects `[opening_balances]`, which would otherwise be silently ignored and
/// drop every opening.
fn moved_to_records<'de, D: serde::Deserializer<'de>>(_: D) -> std::result::Result<(), D::Error> {
    Err(serde::de::Error::custom(
        "[opening_balances] moved to corrected/transactions.csv as transfers with \
         Equity:Opening-Balances",
    ))
}

/// A trip: a date window (plus any bookings paid before leaving) whose records
/// carry a tag, so a whole trip can be totalled across the travel and the sport
/// it spans. Only records already marked `abroad` are swept up by the window,
/// so home spending during the same days is left alone; `include` names the
/// pre-trip bookings the window would miss, and `exclude` drops a record the
/// window would wrongly claim (a private spend, say).
#[derive(Debug, Deserialize)]
pub struct Trip {
    /// One tag or a list, so a trip can be both an activity (`climbing`) and
    /// one named trip (`krabi-2026`) at once — the activity totals every
    /// such trip, the name isolates this one.
    pub tag: TripTags,
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// A trip inside Taiwan has no `abroad` records to key on, so its window
    /// sweeps up every record in range instead. Off by default: an overseas
    /// trip must stay gated on `abroad`, or its window would tag home spending.
    #[serde(default)]
    pub domestic: bool,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// `tag = "climbing"` or `tag = ["climbing", "krabi-2026"]`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TripTags {
    One(String),
    Many(Vec<String>),
}

impl TripTags {
    fn names(&self) -> &[String] {
        match self {
            TripTags::One(s) => std::slice::from_ref(s),
            TripTags::Many(v) => v,
        }
    }
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
    /// Trips whose records pick up a tag. See `Trip`.
    #[serde(default)]
    pub trips: Vec<Trip>,
    /// Subtrees of two-way balances with a person or group. See
    /// `SplitAccounts`.
    #[serde(default)]
    pub split_accounts: SplitAccounts,
    /// The old name of `split_accounts`, refused. See
    /// `renamed_to_split_accounts`.
    #[serde(default, rename = "counterparty", deserialize_with = "renamed_to_split_accounts")]
    _counterparty: (),
    #[serde(default, rename = "opening_balances", deserialize_with = "moved_to_records")]
    _opening_balances: (),
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

    /// The app account mapped to exactly this ledger account, if any.
    pub fn app_account_for(&self, account: &str) -> Result<Option<&str>> {
        let names: Vec<&str> = self
            .accounts
            .iter()
            .filter(|(_, m)| m.account.as_ref() == account)
            .map(|(name, _)| name.as_str())
            .collect();
        match names.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(one)),
            many => anyhow::bail!("{account} is mapped from several app accounts: {many:?}"),
        }
    }

    pub fn override_for(&self, id: &str) -> Option<&Override> {
        self.overrides.get(id).filter(|o| !o.account.is_empty())
    }

    /// Tags a record picks up from the trips it belongs to: a trip that names
    /// it in `include`, or — when the record is `abroad` — one whose window
    /// covers its date. A record listed in a trip's `exclude` is left out
    /// of that trip.
    pub fn trip_tags(&self, date: NaiveDate, id: &str, abroad: bool) -> Vec<String> {
        self.trips
            .iter()
            .filter(|t| {
                let names = |v: &[String]| v.iter().any(|u| u.eq_ignore_ascii_case(id));
                !names(&t.exclude)
                    && (names(&t.include)
                        || ((abroad || t.domestic) && (t.start..=t.end).contains(&date)))
            })
            .flat_map(|t| t.tag.names().iter().map(|s| super::writer::tag_name(s)))
            .collect()
    }

    /// True for an account at or under a split-account root, where a negative
    /// balance means you owe them rather than a missing opening balance.
    pub fn is_split_account(&self, account: &str) -> bool {
        self.split_accounts.roots.iter().any(|root| {
            account
                .strip_prefix(root.as_ref())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
        })
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

#[cfg(test)]
mod trip_tests {
    use super::*;

    fn chart() -> Chart {
        toml::from_str(
            r#"
            [[trips]]
            tag = ["climbing", "krabi-2026"]
            start = "2025-04-01"
            end = "2025-05-31"
            include = ["FLIGHT-UUID"]
            exclude = ["PRIVATE-UUID"]
            "#,
        )
        .expect("valid trips config")
    }

    fn d(s: &str) -> NaiveDate { s.parse().expect("date") }

    /// An abroad record inside the window earns the trip tag; a domestic one on
    /// the same day does not, so home spending during the trip is left alone.
    #[test]
    fn window_tags_only_abroad_records() {
        let c = chart();
        assert_eq!(c.trip_tags(d("2025-04-15"), "X", true), vec!["climbing", "krabi-2026"]);
        assert!(c.trip_tags(d("2025-04-15"), "X", false).is_empty());
        assert!(c.trip_tags(d("2025-06-01"), "X", true).is_empty());
        // A domestic trip sweeps up in-window records even when not abroad.
        let dom: Chart = toml::from_str(
            "[[trips]]\ntag = \"climb\"\nstart = \"2023-01-01\"\nend = \"2023-01-02\"\ndomestic = \
             true\n",
        )
        .expect("valid");
        assert_eq!(dom.trip_tags(d("2023-01-01"), "Y", false), vec!["climb"]);
    }

    /// `include` tags a booking paid before leaving (date outside, not abroad);
    /// `exclude` drops a record the window would otherwise claim.
    #[test]
    fn include_and_exclude_override_the_window() {
        let c = chart();
        assert_eq!(c.trip_tags(d("2025-02-01"), "flight-uuid", false), vec![
            "climbing",
            "krabi-2026"
        ]);
        assert!(c.trip_tags(d("2025-04-15"), "private-uuid", true).is_empty());
    }
}

#[cfg(test)]
mod split_account_tests {
    use super::*;

    /// A root covers itself and everything beneath it, but not a sibling that
    /// merely shares its prefix; with no roots configured nothing qualifies.
    #[test]
    fn roots_cover_their_subtree_only() {
        let c: Chart = toml::from_str("[split_accounts]\nroots = [\"Assets:Split\"]\n")
            .expect("valid split_accounts config");
        assert!(c.is_split_account("Assets:Split"));
        assert!(c.is_split_account("Assets:Split:Friends"));
        assert!(c.is_split_account("Assets:Split:Wisroot:Advance"));
        assert!(!c.is_split_account("Assets:SplitX"));
        assert!(!c.is_split_account("Assets:Cash:TWD"));
        assert!(!Chart::default().is_split_account("Assets:Split:Family"));
    }

    /// A root must still be a valid account path.
    #[test]
    fn rejects_a_root_outside_the_five_roots() {
        assert!(
            toml::from_str::<Chart>("[split_accounts]\nroots = [\"Receivable:Family\"]\n").is_err()
        );
    }

    #[test]
    fn rejects_opening_balances_in_the_config() {
        let err = toml::from_str::<Chart>(
            "[opening_balances]\n\"Assets:Cash\" = { amount = \"1\", date = \"2024-01-01\", \
             currency = \"TWD\" }\n",
        )
        .expect_err("[opening_balances] must not parse");
        assert!(err.to_string().contains("Equity:Opening-Balances"), "{err}");
    }

    /// The section's old name is an error that names the new one, not a
    /// silently ignored table.
    #[test]
    fn rejects_the_old_counterparty_section() {
        let err = toml::from_str::<Chart>("[counterparty]\nroots = [\"Assets:Split\"]\n")
            .expect_err("a stale [counterparty] section must not parse");
        assert!(err.to_string().contains("renamed to [split_accounts]"), "{err}");
    }
}
