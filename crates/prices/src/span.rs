//! Inclusive date ranges, and the questions the rate fetcher asks of them:
//! which days a run of quotes covers, and which days are left.

use std::fmt;

use chrono::{naive::NaiveDateDaysIterator, Duration, NaiveDate};

/// Days without a quote that still count as covered: FX does not trade at
/// weekends or on holidays, and a holiday week with its weekends runs to ten.
pub(crate) const SLACK: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub from: NaiveDate,
    pub through: NaiveDate,
}

impl Span {
    pub fn new(from: NaiveDate, through: NaiveDate) -> Self { Self { from, through } }

    pub(crate) fn day(date: NaiveDate) -> Self { Self::new(date, date) }

    pub(crate) fn days(&self) -> i64 { (self.through - self.from).num_days() + 1 }

    /// True when `held` leaves no stretch of it longer than a holiday.
    pub(crate) fn is_covered_by(&self, held: &[Span]) -> bool {
        uncovered(*self, held).iter().all(|g| g.days() <= SLACK)
    }
}

/// Every day of the span, in order.
impl IntoIterator for Span {
    type IntoIter = std::iter::Take<NaiveDateDaysIterator>;
    type Item = NaiveDate;

    fn into_iter(self) -> Self::IntoIter { self.from.iter_days().take(self.days().max(0) as usize) }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.from == self.through {
            true => write!(f, "{}", self.from),
            false => write!(f, "{} .. {}", self.from, self.through),
        }
    }
}

/// The spans a run of quote dates covers: quotes at most [`SLACK`] days apart
/// join into one span.
pub(crate) fn covered(dates: impl IntoIterator<Item = NaiveDate>) -> Vec<Span> {
    let mut dates: Vec<NaiveDate> = dates.into_iter().collect();
    dates.sort_unstable();
    let mut spans: Vec<Span> = Vec::new();
    for date in dates {
        match spans.last_mut() {
            Some(last) if (date - last.through).num_days() <= SLACK => {
                last.through = last.through.max(date)
            }
            _ => spans.push(Span::day(date)),
        }
    }
    spans
}

/// `spans` sorted, with overlapping or touching spans merged.
pub fn union(spans: impl IntoIterator<Item = Span>) -> Vec<Span> {
    let mut spans: Vec<Span> = spans.into_iter().collect();
    spans.sort_unstable();
    let mut merged: Vec<Span> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.from <= last.through + Duration::days(1) => {
                last.through = last.through.max(span.through)
            }
            _ => merged.push(span),
        }
    }
    merged
}

/// The days both `a` and `b` cover.
pub(crate) fn intersect(a: &[Span], b: &[Span]) -> Vec<Span> {
    let b = union(b.iter().copied());
    union(a.iter().copied())
        .into_iter()
        .flat_map(|x| {
            b.iter()
                .map(move |y| Span::new(x.from.max(y.from), x.through.min(y.through)))
                .filter(|s| s.from <= s.through)
        })
        .collect()
}

/// The parts of `need` that `held` leaves uncovered, in order.
pub(crate) fn uncovered(need: Span, held: &[Span]) -> Vec<Span> {
    let mut left = Vec::new();
    let mut from = need.from;
    for span in union(held.iter().copied()) {
        if span.through < from {
            continue;
        }
        if span.from > need.through {
            break;
        }
        if span.from > from {
            left.push(Span::new(from, span.from - Duration::days(1)));
        }
        from = span.through + Duration::days(1);
    }
    if from <= need.through {
        left.push(Span::new(from, need.through));
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{date as d, span as s};

    #[test]
    fn quotes_join_across_weekends_but_not_months() {
        let dates = ["2024-01-05", "2024-01-08", "2024-01-09", "2024-03-01"].map(d);
        assert_eq!(covered(dates), vec![s("2024-01-05", "2024-01-09"), Span::day(d("2024-03-01"))]);
    }

    #[test]
    fn uncovered_is_what_held_leaves() {
        let need = s("2024-01-01", "2024-01-31");
        let held = [s("2024-01-05", "2024-01-10"), s("2024-01-11", "2024-01-20")];
        assert_eq!(uncovered(need, &held), vec![
            s("2024-01-01", "2024-01-04"),
            s("2024-01-21", "2024-01-31")
        ]);
        assert_eq!(uncovered(need, &[s("2023-01-01", "2025-01-01")]), vec![]);
    }

    #[test]
    fn intersect_keeps_common_days() {
        let a = [s("2024-01-01", "2024-01-10"), s("2024-02-01", "2024-02-10")];
        let b = [s("2024-01-05", "2024-02-03")];
        assert_eq!(intersect(&a, &b), vec![
            s("2024-01-05", "2024-01-10"),
            s("2024-02-01", "2024-02-03")
        ]);
    }
}
