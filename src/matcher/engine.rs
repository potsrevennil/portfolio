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

    /// Validates every leg before mutating, so a rejected proposal leaves the
    /// pool untouched.
    fn commit(&mut self, consumed: &[Consumption]) -> Result<(), CommitError> {
        for c in consumed {
            match self.index_of(c.source) {
                None => return Err(CommitError::UnknownSource(c.source)),
                Some(i) => {
                    let left = self.remaining[i];
                    let overdrawn = if left.is_sign_negative() {
                        c.amount < left || c.amount.is_sign_positive()
                    } else {
                        c.amount > left || c.amount.is_sign_negative()
                    };
                    if overdrawn {
                        return Err(CommitError::Overdrawn {
                            source: c.source,
                            left,
                            want: c.amount,
                        });
                    }
                }
            }
        }
        for c in consumed {
            let i = self.index_of(c.source).expect("validated above");
            self.remaining[i] -= c.amount;
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
    fn pool_rejects_unknown_source() {
        let mut pool = Pool::new(vec![rec(1, "2026-01-01", dec!(-100))]);
        assert_eq!(
            pool.commit(&[Consumption { source: 999, amount: dec!(-10) }]),
            Err(CommitError::UnknownSource(999))
        );
    }
}
