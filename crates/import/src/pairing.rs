//! The one rule a statement line and a 天天記帳 record pair by, whichever
//! arrives first: same account and amount within a week, and only
//! one-to-one. With two candidates either way (two lunches of the same
//! price) nothing chooses; the import stops for the user to review.

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
}
