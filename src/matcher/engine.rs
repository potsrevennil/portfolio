use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::StatementLineMode;

#[derive(Clone, Debug)]
pub struct Record {
    /// Caller's handle, echoed back in [`Match`]; never interpreted here.
    pub ref_id: i64,
    pub date: NaiveDate,
    pub amount: Decimal,
    pub currency: String,
}

/// How much of one candidate a match drew on. An amount rather than a flag so a
/// candidate can be partially consumed across several matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consumption {
    pub source: i64,
    pub amount: Decimal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub mode: &'static str,
    pub target: i64,
    pub consumed: Vec<Consumption>,
    /// Target amount not accounted for; zero for an exact match.
    pub residual: Decimal,
}

impl Match {
    pub fn sources(&self) -> Vec<i64> { self.consumed.iter().map(|c| c.source).collect() }
}

pub struct Pool {
    candidates: Vec<Record>,
    remaining: Vec<Decimal>,
}

pub struct Available<'a> {
    pub record: &'a Record,
    pub remaining: Decimal,
}

impl Pool {
    pub fn new(candidates: Vec<Record>) -> Self {
        let remaining = candidates.iter().map(|c| c.amount).collect();
        Pool { candidates, remaining }
    }

    pub fn available(&self) -> Vec<Available<'_>> {
        self.candidates
            .iter()
            .zip(self.remaining.iter())
            .filter(|(_, rem)| !rem.is_zero())
            .map(|(record, rem)| Available { record, remaining: *rem })
            .collect()
    }

    fn index_of(&self, ref_id: i64) -> Option<usize> {
        self.candidates.iter().position(|c| c.ref_id == ref_id)
    }

    /// Validates the whole proposal before mutating, so a rejected one leaves
    /// the pool untouched. Legs on the same candidate are totalled first, or
    /// two draws that each fit could together overdraw it.
    fn commit(&mut self, consumed: &[Consumption]) -> Result<(), CommitError> {
        let mut totals: BTreeMap<i64, Decimal> = BTreeMap::new();
        for c in consumed {
            *totals.entry(c.source).or_default() += c.amount;
        }

        let mut draws = Vec::with_capacity(totals.len());
        for (&source, &want) in &totals {
            match self.index_of(source) {
                None => return Err(CommitError::UnknownSource(source)),
                Some(i) => {
                    let left = self.remaining[i];
                    let wrong_sign = consumed.iter().any(|c| {
                        c.source == source && c.amount.is_sign_negative() != left.is_sign_negative()
                    });
                    let overdrawn = if left.is_sign_negative() { want < left } else { want > left };
                    if wrong_sign || overdrawn {
                        return Err(CommitError::Overdrawn { source, left, want });
                    }
                    draws.push((i, want));
                }
            }
        }

        for (i, want) in draws {
            self.remaining[i] -= want;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CommitError {
    UnknownSource(i64),
    Overdrawn { source: i64, left: Decimal, want: Decimal },
}

pub trait MatchMode {
    fn name(&self) -> &'static str;

    /// Proposals are applied in order, so a mode must not double-spend a
    /// candidate within its own return value; the engine rejects any that do.
    fn propose(&self, targets: &[Record], pool: &Pool) -> Vec<Match>;
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub matches: Vec<Match>,
    pub unmatched: Vec<i64>,
    pub rejected: Vec<(Match, CommitError)>,
}

/// Modes run in order: exact ones first, so looser modes can't steal their
/// candidates.
pub struct Engine {
    modes: Vec<Box<dyn MatchMode>>,
}

impl Engine {
    pub fn new(modes: Vec<Box<dyn MatchMode>>) -> Self { Engine { modes } }

    pub fn with_default_modes() -> Self { Engine::new(vec![Box::new(StatementLineMode)]) }

    pub fn run(&self, targets: Vec<Record>, candidates: Vec<Record>) -> Outcome {
        let mut pool = Pool::new(candidates);
        let mut matched: Vec<bool> = vec![false; targets.len()];
        let mut outcome = Outcome::default();

        for mode in &self.modes {
            let pending: Vec<Record> = targets
                .iter()
                .zip(matched.iter())
                .filter(|(_, done)| !**done)
                .map(|(t, _)| t.clone())
                .collect();
            if pending.is_empty() {
                break;
            }

            for proposed in mode.propose(&pending, &pool) {
                match pool.commit(&proposed.consumed) {
                    Ok(()) => {
                        if let Some(i) = targets.iter().position(|t| t.ref_id == proposed.target) {
                            matched[i] = true;
                        }
                        outcome.matches.push(proposed);
                    }
                    Err(e) => outcome.rejected.push((proposed, e)),
                }
            }
        }

        outcome.unmatched = targets
            .iter()
            .zip(matched.iter())
            .filter(|(_, done)| !**done)
            .map(|(t, _)| t.ref_id)
            .collect();
        outcome
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn rec(ref_id: i64, date: &str, amount: Decimal) -> Record {
        Record { ref_id, date: date.parse().unwrap(), amount, currency: "TWD".into() }
    }

    #[test]
    fn engine_runs_statement_line_and_reports_unmatched() {
        let targets = vec![rec(1, "2026-01-10", dec!(-12000)), rec(2, "2026-01-11", dec!(-999))];
        let candidates =
            vec![rec(101, "2026-01-10", dec!(-10000)), rec(102, "2026-01-10", dec!(-2000))];

        let outcome = Engine::with_default_modes().run(targets, candidates);

        assert_eq!(outcome.rejected, vec![]);
        assert_eq!(outcome.unmatched, vec![2]);
        assert_eq!(outcome.matches.len(), 1);
        let m = &outcome.matches[0];
        assert_eq!(m.mode, "statement-line");
        assert_eq!(m.target, 1);
        let mut sources = m.sources();
        sources.sort();
        assert_eq!(sources, vec![101, 102]);
        assert_eq!(m.residual, dec!(0));
    }

    #[test]
    fn pool_rejects_overdraw() {
        let mut pool = Pool::new(vec![rec(1, "2026-01-01", dec!(-100))]);
        assert_eq!(pool.commit(&[Consumption { source: 1, amount: dec!(-60) }]), Ok(()));
        assert_eq!(
            pool.commit(&[Consumption { source: 1, amount: dec!(-60) }]),
            Err(CommitError::Overdrawn { source: 1, left: dec!(-40), want: dec!(-60) })
        );
    }

    #[test]
    fn pool_totals_legs_on_the_same_candidate() {
        let mut pool = Pool::new(vec![rec(1, "2026-01-01", dec!(-100))]);
        let two_legs = [Consumption { source: 1, amount: dec!(-60) }, Consumption {
            source: 1,
            amount: dec!(-60),
        }];
        assert_eq!(
            pool.commit(&two_legs),
            Err(CommitError::Overdrawn { source: 1, left: dec!(-100), want: dec!(-120) })
        );
        // Rejected without mutating: the full amount is still available.
        assert_eq!(pool.commit(&[Consumption { source: 1, amount: dec!(-100) }]), Ok(()));
    }

    #[test]
    fn pool_rejects_unknown_source() {
        let mut pool = Pool::new(vec![rec(1, "2026-01-01", dec!(-100))]);
        assert_eq!(
            pool.commit(&[Consumption { source: 999, amount: dec!(-10) }]),
            Err(CommitError::UnknownSource(999))
        );
    }
}
