//! How often an account's own figure (a statement, or a count) should
//! arrive, and where an account stands against it: what the 對帳進度 page
//! reminds the reader of.

use std::fmt;

use chrono::{Datelike, Days, Months, NaiveDate};
use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumString};

/// Days a figure may run past its closing day before it counts as late,
/// unless an account sets its own: a statement is mailed a few days after it
/// closes, and a closing on a holiday moves to the next working day.
pub const GRACE: u32 = 10;

/// How often an account's own figure should arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cadence {
    /// No schedule: never due.
    Never,
    /// A statement each month, closing on `day` (a card's 結數日); a shorter
    /// month closes on its last day, so 31 is the month's end.
    Monthly { day: u32 },
    /// A statement each calendar quarter.
    Quarterly,
    /// A statement each year, closing on `month`/`day`.
    Yearly { month: u32, day: u32 },
    /// A balance confirmed by hand every `every` days: cash counted, or a
    /// loan read off the bank's app.
    Count { every: u32 },
    /// Paid off in instalments through a card or a bank: checked as those
    /// statements show each payment, so never due of its own.
    Instalment,
    /// Repaid every month on `day` (a loan): due is the month's payment
    /// record, not a statement.
    Payment { day: u32 },
}

/// A cadence as stored and as a form sends it: its kind and the numbers that
/// kind takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CadenceSpec {
    pub kind: CadenceKind,
    pub month: Option<u32>,
    pub day: Option<u32>,
    pub every: Option<u32>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Display, EnumString, Serialize, Deserialize,
)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum CadenceKind {
    #[default]
    Never,
    Monthly,
    Quarterly,
    Yearly,
    Count,
    Instalment,
    Payment,
}

impl CadenceKind {
    /// Whether this kind closes on a day of the month.
    pub fn takes_day(self) -> bool {
        matches!(self, CadenceKind::Monthly | CadenceKind::Yearly | CadenceKind::Payment)
    }
}

/// A spec whose numbers its kind cannot take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidCadence {
    Day,
    Month,
    Every,
}

impl fmt::Display for InvalidCadence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            InvalidCadence::Day => "結數日愛是 1 到 31 號",
            InvalidCadence::Month => "月份愛是 1 到 12 月",
            InvalidCadence::Every => "手動對愛隔 1 到 3660 日",
        })
    }
}

impl std::error::Error for InvalidCadence {}

impl TryFrom<CadenceSpec> for Cadence {
    type Error = InvalidCadence;

    fn try_from(s: CadenceSpec) -> Result<Self, InvalidCadence> {
        let day = |d: Option<u32>| match d {
            Some(d @ 1..=31) => Ok(d),
            _ => Err(InvalidCadence::Day),
        };
        Ok(match s.kind {
            CadenceKind::Never => Cadence::Never,
            CadenceKind::Monthly => Cadence::Monthly { day: day(s.day)? },
            CadenceKind::Quarterly => Cadence::Quarterly,
            CadenceKind::Yearly => match s.month {
                Some(month @ 1..=12) => Cadence::Yearly { month, day: day(s.day)? },
                _ => return Err(InvalidCadence::Month),
            },
            CadenceKind::Count => match s.every {
                Some(every @ 1..=3660) => Cadence::Count { every },
                _ => return Err(InvalidCadence::Every),
            },
            CadenceKind::Instalment => Cadence::Instalment,
            CadenceKind::Payment => Cadence::Payment { day: day(s.day)? },
        })
    }
}

impl From<Cadence> for CadenceSpec {
    fn from(c: Cadence) -> Self {
        let spec = |kind| CadenceSpec { kind, ..CadenceSpec::default() };
        match c {
            Cadence::Never => spec(CadenceKind::Never),
            Cadence::Monthly { day } => {
                CadenceSpec { day: Some(day), ..spec(CadenceKind::Monthly) }
            }
            Cadence::Quarterly => spec(CadenceKind::Quarterly),
            Cadence::Yearly { month, day } => {
                CadenceSpec { month: Some(month), day: Some(day), ..spec(CadenceKind::Yearly) }
            }
            Cadence::Count { every } => {
                CadenceSpec { every: Some(every), ..spec(CadenceKind::Count) }
            }
            Cadence::Instalment => spec(CadenceKind::Instalment),
            Cadence::Payment { day } => {
                CadenceSpec { day: Some(day), ..spec(CadenceKind::Payment) }
            }
        }
    }
}

impl Cadence {
    /// Checked by importing statements rather than by hand.
    pub fn by_statement(self) -> bool {
        matches!(self, Cadence::Monthly { .. } | Cadence::Quarterly | Cadence::Yearly { .. })
    }

    /// The newest closing before `today`: the day the newest figure should
    /// have reached. `None` when this cadence never makes one due.
    fn due(self, today: NaiveDate) -> Option<NaiveDate> {
        match self {
            Cadence::Never | Cadence::Instalment => None,
            Cadence::Monthly { day } | Cadence::Payment { day } => {
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
            Cadence::Yearly { month, day } => {
                let this_year = closing(today.year(), month, day);
                match this_year < today {
                    true => Some(this_year),
                    false => Some(closing(today.year() - 1, month, day)),
                }
            }
            Cadence::Count { every } => today.checked_sub_days(Days::new(every.into())),
        }
    }

    /// The first closing after `day`: where an account whose figures stop
    /// at `day` started being late.
    fn due_after(self, day: NaiveDate) -> Option<NaiveDate> {
        match self {
            Cadence::Never | Cadence::Instalment => None,
            Cadence::Monthly { day: d } | Cadence::Payment { day: d } => {
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
            Cadence::Yearly { month, day: d } => {
                let this_year = closing(day.year(), month, d);
                match this_year > day {
                    true => Some(this_year),
                    false => Some(closing(day.year() + 1, month, d)),
                }
            }
            Cadence::Count { every } => day.checked_add_days(Days::new(every.into())),
        }
    }

    /// The figures missing by `today` when the last one reached `checked`.
    pub fn missed(self, checked: Option<NaiveDate>, today: NaiveDate) -> Option<Missed> {
        self.due(today)
            .filter(|due| checked.is_none_or(|c| c < *due))
            .map(|due| Missed { since: checked.and_then(|c| self.due_after(c)), due })
    }

    /// Where an account stands when its last figure reached `checked`.
    /// Lateness counts from the first figure missed, not the latest, so an
    /// account missing a year of statements is never let off because its
    /// newest one is only just due.
    pub fn standing(self, checked: Option<NaiveDate>, grace: u32, today: NaiveDate) -> Standing {
        match (self, self.missed(checked, today)) {
            (Cadence::Never | Cadence::Instalment, _) => Standing::Unscheduled,
            (_, None) => Standing::Current,
            (_, Some(m)) if checked.is_none() || m.late_days(today) > i64::from(grace) => {
                Standing::Overdue
            }
            (_, Some(_)) => Standing::Due,
        }
    }
}

/// Figures a cadence asked for that have not arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Missed {
    /// The first one missed after the last figure; `None` when there never
    /// was one.
    pub since: Option<NaiveDate>,
    /// The newest one due.
    pub due: NaiveDate,
}

impl Missed {
    /// Days since the first one missed.
    pub fn late_days(self, today: NaiveDate) -> i64 {
        (today - self.since.unwrap_or(self.due)).num_days()
    }
}

/// `day` of the month, or its last day when the month is shorter.
fn closing(year: i32, month: u32, day: u32) -> NaiveDate {
    (1..=day)
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month, d))
        .expect("every month has a first day")
}

/// Where an account stands against its cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, Serialize, Deserialize)]
#[strum(serialize_all = "lowercase")]
pub enum Standing {
    /// Vouched for through its newest closing.
    Current,
    /// A figure is due, within the grace days.
    Due,
    /// A due figure is later than the grace days, or none ever came.
    Overdue,
    /// Its cadence never makes a figure due.
    Unscheduled,
}

impl Standing {
    /// A figure is due, whether within the grace days or past them.
    pub fn is_due(self) -> bool { matches!(self, Standing::Due | Standing::Overdue) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate { s.parse().unwrap() }

    fn stand(cadence: Cadence, checked: Option<&str>, today: &str) -> Standing {
        cadence.standing(checked.map(date), GRACE, date(today))
    }

    #[test]
    fn a_monthly_statement_is_due_from_the_day_after_it_closes() {
        let card = Cadence::Monthly { day: 25 };
        assert_eq!(card.due(date("2026-10-25")), Some(date("2026-09-25")));
        assert_eq!(card.due(date("2026-10-26")), Some(date("2026-10-25")));
        assert_eq!(card.due(date("2026-01-10")), Some(date("2025-12-25")));
    }

    #[test]
    fn a_short_month_closes_on_its_last_day() {
        let month_end = Cadence::Monthly { day: 31 };
        assert_eq!(month_end.due(date("2026-03-15")), Some(date("2026-02-28")));
        assert_eq!(month_end.due(date("2026-05-01")), Some(date("2026-04-30")));
    }

    #[test]
    fn other_cadences() {
        assert_eq!(Cadence::Quarterly.due(date("2026-10-18")), Some(date("2026-09-30")));
        assert_eq!(Cadence::Quarterly.due(date("2026-10-01")), Some(date("2026-09-30")));
        let year_end = Cadence::Yearly { month: 12, day: 31 };
        assert_eq!(year_end.due(date("2026-10-18")), Some(date("2025-12-31")));
        assert_eq!(year_end.due(date("2027-01-01")), Some(date("2026-12-31")));
        assert_eq!(Cadence::Never.due(date("2026-10-18")), None);
        assert_eq!(Cadence::Count { every: 30 }.due(date("2026-10-18")), Some(date("2026-09-18")));
    }

    #[test]
    fn the_first_closing_after_a_figure() {
        let card = Cadence::Monthly { day: 25 };
        assert_eq!(card.due_after(date("2026-08-25")), Some(date("2026-09-25")));
        assert_eq!(card.due_after(date("2026-08-24")), Some(date("2026-08-25")));
        assert_eq!(Cadence::Quarterly.due_after(date("2026-06-30")), Some(date("2026-09-30")));
        assert_eq!(Cadence::Quarterly.due_after(date("2026-05-02")), Some(date("2026-06-30")));
        let year_end = Cadence::Yearly { month: 12, day: 31 };
        assert_eq!(year_end.due_after(date("2025-12-31")), Some(date("2026-12-31")));
        assert_eq!(
            Cadence::Count { every: 30 }.due_after(date("2026-09-01")),
            Some(date("2026-10-01"))
        );
        assert_eq!(Cadence::Never.due_after(date("2026-09-01")), None);
    }

    #[test]
    fn what_was_missed() {
        let card = Cadence::Monthly { day: 25 };
        let today = date("2026-10-18");
        assert_eq!(card.missed(Some(date("2026-09-25")), today), None);
        assert_eq!(
            card.missed(Some(date("2026-07-25")), today),
            Some(Missed { since: Some(date("2026-08-25")), due: date("2026-09-25") })
        );
        assert_eq!(card.missed(None, today), Some(Missed { since: None, due: date("2026-09-25") }));
        // A count interval past the calendar is never due rather than a panic.
        assert_eq!(Cadence::Count { every: u32::MAX }.due(today), None);
    }

    #[test]
    fn standing() {
        let quarterly = |checked| stand(Cadence::Quarterly, checked, "2026-10-08");
        assert_eq!(quarterly(Some("2026-09-30")), Standing::Current);
        assert_eq!(quarterly(Some("2026-06-30")), Standing::Due);
        assert_eq!(quarterly(None), Standing::Overdue);
        // A year of missing statements is late, though the newest just fell due.
        let card = Cadence::Monthly { day: 15 };
        assert_eq!(stand(card, Some("2026-09-15"), "2026-10-18"), Standing::Due);
        assert_eq!(stand(card, Some("2025-09-15"), "2026-10-18"), Standing::Overdue);
        assert_eq!(stand(Cadence::Never, None, "2026-10-18"), Standing::Unscheduled);
    }

    #[test]
    fn a_cadence_round_trips_through_its_spec() {
        for c in [
            Cadence::Never,
            Cadence::Monthly { day: 17 },
            Cadence::Quarterly,
            Cadence::Yearly { month: 12, day: 31 },
            Cadence::Count { every: 30 },
            Cadence::Instalment,
            Cadence::Payment { day: 20 },
        ] {
            assert_eq!(Cadence::try_from(CadenceSpec::from(c)).unwrap(), c);
        }
    }

    #[test]
    fn a_spec_missing_its_day_is_refused() {
        let spec = |kind, month, day, every| CadenceSpec { kind, month, day, every };
        for bad in [
            spec(CadenceKind::Monthly, None, None, None),
            spec(CadenceKind::Monthly, None, Some(32), None),
            spec(CadenceKind::Yearly, Some(13), Some(1), None),
            spec(CadenceKind::Count, None, None, Some(0)),
        ] {
            assert!(Cadence::try_from(bad).is_err(), "{bad:?}");
        }
    }
}
