//! Where each account's outside figure comes from and how often it should
//! arrive: the `[[sources]]` of mapping.toml, which the coverage page reads to
//! tell a statement that is merely not due yet from one that is late.

use std::{fs, path::Path};

use anyhow::{bail, Context, Result};
use chrono::{Datelike, Days, Months, NaiveDate};
use serde::Deserialize;

use crate::accounts::Account;

/// Days a figure may run past its due day before it counts as late, unless a
/// source sets its own: a card statement is mailed a few days after it closes.
const GRACE: u32 = 10;

/// How often a source's own figure should arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// A statement each month, closing on `day` (a card's 結帳日); a shorter
    /// month closes on its last day.
    Monthly { day: u32 },
    /// A statement each calendar quarter.
    Quarterly,
    /// Downloaded when wanted, as a bank export is: never late.
    OnDemand,
    /// Current at any moment (Splitwise): due through yesterday.
    Live,
    /// A balance counted by hand every `every` days.
    Count { every: u32 },
}

impl Cadence {
    /// The day the newest figure should have reached by `today`, if this
    /// cadence ever makes one due.
    pub fn due(self, today: NaiveDate) -> Option<NaiveDate> {
        match self {
            Cadence::Monthly { day } => {
                let this_month = closing(today.year(), today.month(), day);
                match this_month < today {
                    true => Some(this_month),
                    false => {
                        let last = today.with_day(1)? - Days::new(1);
                        Some(closing(last.year(), last.month(), day))
                    }
                }
            }
            Cadence::Quarterly => {
                let first = NaiveDate::from_ymd_opt(today.year(), (today.month0() / 3) * 3 + 1, 1)?;
                Some(first - Days::new(1))
            }
            Cadence::OnDemand => None,
            Cadence::Live => Some(today - Days::new(1)),
            Cadence::Count { every } => Some(today - Days::new(every.into())),
        }
    }

    /// The first day after `day` a figure falls due: where a source whose
    /// figures stop at `day` started being late.
    pub fn due_after(self, day: NaiveDate) -> Option<NaiveDate> {
        match self {
            Cadence::Monthly { day: d } => {
                let this_month = closing(day.year(), day.month(), d);
                match this_month > day {
                    true => Some(this_month),
                    false => {
                        let next = this_month.with_day(1)? + Months::new(1);
                        Some(closing(next.year(), next.month(), d))
                    }
                }
            }
            // The end of the quarter `day` is in, or of the next if it is that end.
            Cadence::Quarterly => Cadence::Quarterly.due(day + Days::new(1) + Months::new(3)),
            Cadence::OnDemand => None,
            Cadence::Live => Some(day + Days::new(1)),
            Cadence::Count { every } => Some(day + Days::new(every.into())),
        }
    }
}

/// `day` of the month, or its last day when the month is shorter.
fn closing(year: i32, month: u32, day: u32) -> NaiveDate {
    (1..=day)
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month, d))
        .expect("every month has a first day")
}

/// Where a source stands against its cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Vouched for through its due day, and nothing recorded since.
    Checked,
    /// Records after the last figure, which nothing has checked yet.
    Unchecked,
    /// A figure is due, or none ever arrived, within the grace days.
    Due,
    /// A due figure is later than the grace days.
    Overdue,
}

impl Standing {
    /// `checked` is the day a figure last vouched for the source through;
    /// `tail` says records sit after it. Lateness counts from the first figure
    /// missed, not the latest, so a source missing a year of statements is
    /// never let off because its newest one is only just due.
    pub fn of(
        cadence: Cadence,
        grace: u32,
        checked: Option<NaiveDate>,
        tail: bool,
        today: NaiveDate,
    ) -> Self {
        let due = cadence.due(today).filter(|due| checked.is_none_or(|c| c < *due));
        let missed_since = match checked {
            Some(c) => due.and_then(|_| cadence.due_after(c)),
            None => due,
        };
        let late = |since: NaiveDate| (today - since).num_days() > i64::from(grace);
        match (missed_since, checked, tail) {
            // Records with no figure at all: late whenever one is due.
            (Some(_), None, true) => Standing::Overdue,
            (Some(since), ..) if late(since) => Standing::Overdue,
            (Some(_), ..) | (None, None, false) => Standing::Due,
            (None, _, true) => Standing::Unchecked,
            (None, Some(_), false) => Standing::Checked,
        }
    }
}

/// One source: an account (and its subtree) and its cadence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub account: String,
    /// Shown in place of the account's own label.
    pub label: Option<String>,
    pub cadence: Cadence,
    pub grace: u32,
    /// Its folder under `raw/`, as `raw/manifest.csv` lists the files.
    pub raw: Option<String>,
    /// Where its statements are downloaded.
    pub url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    Monthly,
    Quarterly,
    OnDemand,
    Live,
    Count,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    account: Account,
    label: Option<String>,
    cadence: Kind,
    /// Monthly: the closing day.
    day: Option<u32>,
    /// Count: days between counts.
    every: Option<u32>,
    grace: Option<u32>,
    raw: Option<String>,
    url: Option<String>,
}

impl TryFrom<Entry> for Source {
    type Error = anyhow::Error;

    fn try_from(e: Entry) -> Result<Self> {
        let cadence = match (e.cadence, e.day, e.every) {
            (Kind::Monthly, Some(day @ 1..=31), None) => Cadence::Monthly { day },
            (Kind::Monthly, ..) => bail!("monthly needs day = 1..31, and no every"),
            (Kind::Count, None, Some(every @ 1..)) => Cadence::Count { every },
            (Kind::Count, ..) => bail!("count needs every = days between counts, and no day"),
            (_, Some(_), _) | (_, _, Some(_)) => bail!("only monthly takes day, only count every"),
            (Kind::Quarterly, None, None) => Cadence::Quarterly,
            (Kind::OnDemand, None, None) => Cadence::OnDemand,
            (Kind::Live, None, None) => Cadence::Live,
        };
        if e.account.is_empty() {
            bail!("account is required");
        }
        Ok(Source {
            account: e.account.to_string(),
            label: e.label,
            cadence,
            grace: e.grace.unwrap_or(GRACE),
            raw: e.raw.map(|r| r.trim_matches('/').to_string()),
            url: e.url,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    sources: Vec<Entry>,
}

/// The configured sources, in config order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sources(pub Vec<Source>);

impl Sources {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        text.parse().with_context(|| format!("parsing {}", path.display()))
    }
}

/// The file is shared with the chart, so other sections are ignored.
impl std::str::FromStr for Sources {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        let sources = config
            .sources
            .into_iter()
            .map(|e| {
                let account = e.account.to_string();
                Source::try_from(e).with_context(|| format!("[[sources]] {account}"))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut seen = std::collections::BTreeSet::new();
        match sources.iter().find(|s| !seen.insert(&s.account)) {
            Some(twice) => bail!("[[sources]] lists {} twice", twice.account),
            None => Ok(Sources(sources)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate { s.parse().unwrap() }

    #[test]
    fn a_monthly_statement_is_due_from_the_day_after_it_closes() {
        let card = Cadence::Monthly { day: 25 };
        assert_eq!(card.due(date("2026-10-25")), Some(date("2026-09-25")));
        assert_eq!(card.due(date("2026-10-26")), Some(date("2026-10-25")));
        assert_eq!(card.due(date("2026-01-10")), Some(date("2025-12-25")));
    }

    #[test]
    fn a_short_month_closes_on_its_last_day() {
        let card = Cadence::Monthly { day: 31 };
        assert_eq!(card.due(date("2026-03-15")), Some(date("2026-02-28")));
        assert_eq!(card.due(date("2026-05-01")), Some(date("2026-04-30")));
    }

    #[test]
    fn other_cadences() {
        assert_eq!(Cadence::Quarterly.due(date("2026-10-18")), Some(date("2026-09-30")));
        assert_eq!(Cadence::Quarterly.due(date("2026-10-01")), Some(date("2026-09-30")));
        assert_eq!(Cadence::Quarterly.due(date("2026-03-31")), Some(date("2025-12-31")));
        assert_eq!(Cadence::OnDemand.due(date("2026-10-18")), None);
        assert_eq!(Cadence::Live.due(date("2026-10-18")), Some(date("2026-10-17")));
        assert_eq!(Cadence::Count { every: 30 }.due(date("2026-10-18")), Some(date("2026-09-18")));
    }

    #[test]
    fn the_first_due_day_after_a_figure() {
        let card = Cadence::Monthly { day: 25 };
        assert_eq!(card.due_after(date("2026-08-25")), Some(date("2026-09-25")));
        assert_eq!(card.due_after(date("2026-08-24")), Some(date("2026-08-25")));
        assert_eq!(Cadence::Quarterly.due_after(date("2026-06-30")), Some(date("2026-09-30")));
        assert_eq!(Cadence::Quarterly.due_after(date("2026-05-02")), Some(date("2026-06-30")));
        assert_eq!(Cadence::Live.due_after(date("2026-10-01")), Some(date("2026-10-02")));
        assert_eq!(
            Cadence::Count { every: 30 }.due_after(date("2026-09-01")),
            Some(date("2026-10-01"))
        );
        assert_eq!(Cadence::OnDemand.due_after(date("2026-09-01")), None);
    }

    #[test]
    fn standing() {
        let today = date("2026-10-18");
        let of = |checked: Option<&str>, tail, grace| {
            Standing::of(Cadence::Quarterly, grace, checked.map(date), tail, today)
        };
        assert_eq!(of(Some("2026-09-30"), false, 10), Standing::Checked);
        assert_eq!(of(Some("2026-09-30"), true, 10), Standing::Unchecked);
        assert_eq!(of(Some("2026-06-30"), true, 30), Standing::Due);
        assert_eq!(of(Some("2026-06-30"), false, 10), Standing::Overdue);
        assert_eq!(of(None, true, 30), Standing::Overdue);
        assert_eq!(of(None, false, 30), Standing::Due);

        // A year of missing statements is late, though the newest just fell due.
        let card = Cadence::Monthly { day: 15 };
        let of = |checked: &str| Standing::of(card, 10, Some(date(checked)), false, today);
        assert_eq!(of("2026-09-15"), Standing::Due);
        assert_eq!(of("2025-09-15"), Standing::Overdue);

        let live =
            |checked: &str| Standing::of(Cadence::Live, 10, Some(date(checked)), false, today);
        assert_eq!(live("2026-10-17"), Standing::Checked);
        assert_eq!(live("2026-10-10"), Standing::Due);
        assert_eq!(live("2026-09-30"), Standing::Overdue);

        // Never late on demand, but nothing at all is still to fetch.
        let on_demand = |checked: Option<&str>, tail| {
            Standing::of(Cadence::OnDemand, 10, checked.map(date), tail, today)
        };
        assert_eq!(on_demand(Some("2020-01-01"), false), Standing::Checked);
        assert_eq!(on_demand(Some("2020-01-01"), true), Standing::Unchecked);
        assert_eq!(on_demand(None, true), Standing::Unchecked);
        assert_eq!(on_demand(None, false), Standing::Due);
    }

    #[test]
    fn parses_sources() {
        let sources: Sources = r#"
            [accounts]
            "x" = "Assets:Cash"

            [[sources]]
            account = "Liabilities:Card:Alpha"
            cadence = "monthly"
            day     = 25
            raw     = "alpha-card/"
            url     = "https://bank.example/statements"

            [[sources]]
            account = "Assets:Cash"
            label   = "現金"
            cadence = "count"
            every   = 30
            grace   = 5
        "#
        .parse()
        .unwrap();
        assert_eq!(sources.0, vec![
            Source {
                account: "Liabilities:Card:Alpha".into(),
                label: None,
                cadence: Cadence::Monthly { day: 25 },
                grace: GRACE,
                raw: Some("alpha-card".into()),
                url: Some("https://bank.example/statements".into()),
            },
            Source {
                account: "Assets:Cash".into(),
                label: Some("現金".into()),
                cadence: Cadence::Count { every: 30 },
                grace: 5,
                raw: None,
                url: None,
            },
        ]);
    }

    #[test]
    fn refuses_a_cadence_missing_its_day() {
        for bad in [
            r#"[[sources]]
               account = "Assets:Bank"
               cadence = "monthly""#,
            r#"[[sources]]
               account = "Assets:Bank"
               cadence = "on-demand"
               day = 3"#,
            r#"[[sources]]
               account = "Assets:Bank"
               cadence = "weekly""#,
            r#"[[sources]]
               account = "Assets:Bank"
               cadence = "live"
               cadance = "live""#,
            r#"[[sources]]
               account = "Assets:Bank"
               cadence = "live"
               [[sources]]
               account = "Assets:Bank"
               cadence = "live""#,
        ] {
            assert!(bad.parse::<Sources>().is_err(), "{bad}");
        }
    }
}
