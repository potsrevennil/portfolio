//! The one rule a statement line and a 天天記帳 record pair by, whichever
//! arrives first: same account and amount within a week. Only a fit that is
//! one-to-one is certain; with two candidates either way (two lunches of the
//! same price) the nearest in date is the import's best reading, for a person
//! to look at.

use std::collections::HashSet;

use chrono::NaiveDate;

/// How far a record's date may be from its line: the records take the bank's
/// date when they are a week or more off.
pub const WINDOW_DAYS: i64 = 7;

pub fn within_window(a: NaiveDate, b: NaiveDate) -> bool { (a - b).num_days().abs() <= WINDOW_DAYS }

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pairing {
    /// (left, right) pairs that fit only each other.
    pub pairs: Vec<(usize, usize)>,
    /// Each left item with the right items it fits, when it fits several or
    /// its one fits another left item too.
    pub ambiguous: Vec<(usize, Vec<usize>)>,
}

impl Pairing {
    /// Settles [`Self::ambiguous`] one-to-one, the pairs nearest by `distance`
    /// first. An item left with nothing stays unpaired.
    pub fn nearest(&self, distance: impl Fn(usize, usize) -> i64) -> Vec<(usize, usize)> {
        let mut fits: Vec<(i64, usize, usize)> = self
            .ambiguous
            .iter()
            .flat_map(|(l, many)| many.iter().map(move |&r| (*l, r)))
            .map(|(l, r)| (distance(l, r), l, r))
            .collect();
        fits.sort_unstable();
        let (mut lefts, mut rights) = (HashSet::new(), HashSet::new());
        fits.into_iter()
            .filter(|&(_, l, r)| {
                let free = !lefts.contains(&l) && !rights.contains(&r);
                if free {
                    lefts.insert(l);
                    rights.insert(r);
                }
                free
            })
            .map(|(_, l, r)| (l, r))
            .collect()
    }
}

pub fn one_to_one<L, R>(left: &[L], right: &[R], fits: impl Fn(&L, &R) -> bool) -> Pairing {
    let mut pairing = Pairing::default();
    for (li, l) in left.iter().enumerate() {
        let fitting: Vec<usize> = (0..right.len()).filter(|&ri| fits(l, &right[ri])).collect();
        let rivals = |ri: usize| left.iter().filter(|o| fits(o, &right[ri])).count();
        match fitting.as_slice() {
            [] => {}
            [only] if rivals(*only) == 1 => pairing.pairs.push((li, *only)),
            _ => pairing.ambiguous.push((li, fitting)),
        }
    }
    pairing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_unique_fit_both_ways_pairs() {
        let fits = |a: &i32, b: &i32| a == b;
        assert_eq!(one_to_one(&[1, 2], &[2, 3], fits), Pairing {
            pairs: vec![(1, 0)],
            ambiguous: vec![]
        });
        // Two lines, one record: neither line may take it.
        assert_eq!(one_to_one(&[5, 5], &[5], fits), Pairing {
            pairs: vec![],
            ambiguous: vec![(0, vec![0]), (1, vec![0])]
        });
        assert_eq!(one_to_one(&[5], &[5, 5], fits).ambiguous, [(0, vec![0, 1])]);
    }

    #[test]
    fn the_nearest_settle_what_nothing_else_chooses() {
        let days = [1_i64, 10];
        let records = [9_i64, 2];
        let pairing = one_to_one(&days, &records, |_, _| true);
        let near = pairing.nearest(|l, r| (days[l] - records[r]).abs());
        assert_eq!(near, [(0, 1), (1, 0)]);
        // Two lines, one record: the nearer line takes it, the other is left.
        let pairing = one_to_one(&days, &records[..1], |_, _| true);
        assert_eq!(pairing.nearest(|l, r| (days[l] - records[r]).abs()), [(1, 0)]);
    }
}
