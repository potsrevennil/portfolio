//! Matches bank statement lines against what 天天記帳 recorded.
//!
//! Not one-to-one: a single bank transfer is often split across several
//! categories in the app (12,000 recorded as 10,000 rent + 2,000 utilities),
//! and dates differ because securities settle T+2 while the app records the
//! trade date. So this looks for a *subset* of app records that sums to the
//! bank line, widening the date tolerance over successive passes.

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

fn search(
    pool: &[usize],
    events: &[(NaiveDate, Decimal)],
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
    lines: &[(NaiveDate, Decimal)],
    events: &[(NaiveDate, Decimal)],
) -> Vec<Option<Vec<usize>>> {
    let mut assigned: Vec<Option<Vec<usize>>> = vec![None; lines.len()];
    let mut used = vec![false; events.len()];

    for tol in TOLERANCES {
        for (li, (date, delta)) in lines.iter().enumerate() {
            if assigned[li].is_some() || delta.is_zero() {
                continue;
            }

            let mut pool: Vec<usize> = events
                .iter()
                .enumerate()
                .filter(|(ei, (edate, edelta))| {
                    !used[*ei]
                        && !edelta.is_zero()
                        && edelta.is_sign_negative() == delta.is_sign_negative()
                        && (*edate - *date).num_days().abs() <= tol
                })
                .map(|(ei, _)| ei)
                .collect();
            pool.sort_by_key(|ei| (events[*ei].0 - *date).num_days().abs());
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
