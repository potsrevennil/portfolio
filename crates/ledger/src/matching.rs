//! Matches bank statement lines against what 天天記帳 recorded.
//!
//! Not one-to-one: a single bank transfer is often split across several
//! categories in the app (12,000 recorded as 10,000 rent + 2,000 utilities),
//! and dates differ because securities settle T+2 while the app records the
//! trade date. So this looks for a *subset* of app records that sums to the
//! bank line, widening the date tolerance over successive passes. A card line
//! spans its purchase to its posting, and a record anywhere in that is on time.

use chrono::NaiveDate;
use rust_decimal::Decimal;

/// Widening passes. Early passes claim the unambiguous same-day matches before
/// looser ones get a chance to steal them.
const TOLERANCES: [i64; 5] = [0, 2, 5, 12, MAX_TOLERANCE];
/// Furthest apart, in days, a record and the bank line it explains may be.
pub const MAX_TOLERANCE: i64 = 45;
/// Cap on candidates considered per line, nearest date first.
const POOL_CAP: usize = 14;
/// Largest number of app records allowed to make up one bank line.
const MAX_SUBSET: usize = 4;

/// The days something may have happened on: one, or a card line's purchase
/// through its posting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateRange {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

impl From<NaiveDate> for DateRange {
    fn from(day: NaiveDate) -> Self { DateRange { from: day, to: day } }
}

/// Either way round: a statement can print a posting before its purchase.
impl From<(NaiveDate, NaiveDate)> for DateRange {
    fn from((a, b): (NaiveDate, NaiveDate)) -> Self { DateRange { from: a.min(b), to: a.max(b) } }
}

impl DateRange {
    /// Days between the two; zero when they overlap.
    pub fn distance(self, other: DateRange) -> i64 {
        let after = |a: NaiveDate, b: NaiveDate| (b - a).num_days().max(0);
        after(self.to, other.from).max(after(other.to, self.from))
    }
}

fn search(
    pool: &[usize],
    events: &[(DateRange, Decimal)],
    remaining: Decimal,
    size: usize,
    start: usize,
    chosen: &mut Vec<usize>,
) -> Option<Vec<usize>> {
    if size == 0 {
        remaining.is_zero().then(|| chosen.clone())
    } else {
        let mut found = None;
        for i in start..pool.len() {
            chosen.push(pool[i]);
            found = search(pool, events, remaining - events[pool[i]].1, size - 1, i + 1, chosen);
            chosen.pop();
            if found.is_some() {
                break;
            }
        }
        found
    }
}

/// For each line, the indices of `events` summing to it; `None` if unmatched.
/// Shared by the bake and the matcher's statement-line mode.
pub fn match_subsets(
    lines: &[(DateRange, Decimal)],
    events: &[(DateRange, Decimal)],
) -> Vec<Option<Vec<usize>>> {
    let mut assigned: Vec<Option<Vec<usize>>> = vec![None; lines.len()];
    let mut used = vec![false; events.len()];

    for tol in TOLERANCES {
        for (li, (dates, delta)) in lines.iter().enumerate() {
            if assigned[li].is_some() || delta.is_zero() {
                continue;
            }

            let mut pool: Vec<usize> = events
                .iter()
                .enumerate()
                .filter(|(ei, (edates, edelta))| {
                    !used[*ei]
                        && !edelta.is_zero()
                        && edelta.is_sign_negative() == delta.is_sign_negative()
                        && dates.distance(*edates) <= tol
                })
                .map(|(ei, _)| ei)
                .collect();
            // Among records on time, the one nearest the purchase: repeats of
            // one amount then pair up in order, not crosswise.
            pool.sort_by_key(|ei| {
                let edates = events[*ei].0;
                (dates.distance(edates), (edates.from - dates.from).num_days().abs())
            });
            pool.truncate(POOL_CAP);

            let mut chosen = Vec::new();
            let mut found = None;
            for size in 1..=MAX_SUBSET.min(pool.len()) {
                found = search(&pool, events, *delta, size, 0, &mut chosen);
                if found.is_some() {
                    break;
                }
            }

            if let Some(subset) = found {
                for ei in &subset {
                    used[*ei] = true;
                }
                assigned[li] = Some(subset);
            }
        }
    }

    assigned
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn day(d: &str) -> NaiveDate { d.parse().unwrap() }

    fn range(from: &str, to: &str) -> DateRange { (day(from), day(to)).into() }

    #[test]
    fn ranges_are_zero_apart_on_overlap_and_days_apart_otherwise() {
        let line = range("2026-01-10", "2026-01-15");
        assert_eq!(line.distance(day("2026-01-10").into()), 0);
        assert_eq!(line.distance(day("2026-01-15").into()), 0);
        assert_eq!(line.distance(day("2026-01-09").into()), 1);
        assert_eq!(line.distance(day("2026-01-18").into()), 3);
        assert_eq!(line.distance(range("2026-01-14", "2026-01-20")), 0);
        assert_eq!(range("2026-01-20", "2026-01-21").distance(line), 5);
    }

    #[test]
    fn a_reversed_pair_of_days_still_makes_a_range() {
        assert_eq!(range("2026-01-15", "2026-01-10"), range("2026-01-10", "2026-01-15"));
    }

    /// Single-day lines pair exactly as they did before ranges: nearest first.
    #[test]
    fn single_days_pair_nearest_first() {
        let lines = [(day("2026-02-03").into(), dec!(-50))];
        let events = [(day("2026-01-20").into(), dec!(-50)), (day("2026-02-01").into(), dec!(-50))];
        assert_eq!(match_subsets(&lines, &events), vec![Some(vec![1])]);
    }

    /// By posting date alone the first line sits nearer the second record and
    /// takes it, leaving the second line the first.
    #[test]
    fn repeats_of_one_amount_pair_in_order_across_their_ranges() {
        let lines = [
            (range("2026-01-10", "2026-01-15"), dec!(-29)),
            (range("2026-01-14", "2026-01-16"), dec!(-29)),
        ];
        let events = [(day("2026-01-14").into(), dec!(-29)), (day("2026-01-10").into(), dec!(-29))];
        assert_eq!(match_subsets(&lines, &events), vec![Some(vec![1]), Some(vec![0])]);
    }
}
