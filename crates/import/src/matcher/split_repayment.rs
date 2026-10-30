//! Suggests which of a person's open splits a repayment settles. Only ever a
//! suggestion: the user confirms through `db::bill_split::attach`.

use anyhow::Result;
use db::{
    bill_split::{open_for, unallocated, Books, OpenShare, Unallocated},
    SqlitePool,
};
use rust_decimal::Decimal;

use super::{Consumption, Engine, Match, MatchMode, Pool, Record};

pub const MODE: &str = "split-repayment";

/// No date window: either side may arrive months apart. An exact amount wins,
/// nearest in time; otherwise the oldest debts are repaid first, as far as the
/// repayment goes. Candidates are what is owed and targets what came in, so
/// their signs are opposite.
#[derive(Default)]
pub struct SplitRepaymentMode;

impl MatchMode for SplitRepaymentMode {
    fn name(&self) -> &'static str { MODE }

    fn propose(&self, targets: &[Record], pool: &Pool) -> Vec<Match> {
        let mut left: Vec<(&Record, Decimal)> =
            pool.available().into_iter().map(|a| (a.record, a.remaining)).collect();
        left.sort_by_key(|(record, _)| record.date);

        let mut exact = vec![None; targets.len()];
        for (i, target) in targets.iter().enumerate() {
            let nearest = left
                .iter()
                .enumerate()
                .filter(|(_, (c, rem))| c.currency == target.currency && *rem == -target.amount)
                .min_by_key(|(_, (c, _))| (c.date - target.date).num_days().abs())
                .map(|(at, _)| at);
            if let Some(at) = nearest {
                exact[i] = Some(Consumption { source: left[at].0.ref_id, amount: left[at].1 });
                left[at].1 = Decimal::ZERO;
            }
        }

        let mut matches = Vec::new();
        for (target, exact) in targets.iter().zip(exact) {
            let consumed = match exact {
                Some(whole) => vec![whole],
                None => oldest_first(target, &mut left),
            };
            if !consumed.is_empty() {
                let drawn: Decimal = consumed.iter().map(|c| c.amount).sum();
                matches.push(Match {
                    mode: MODE,
                    target: target.ref_id,
                    consumed,
                    residual: target.amount + drawn,
                });
            }
        }
        matches
    }
}

fn oldest_first(target: &Record, left: &mut [(&Record, Decimal)]) -> Vec<Consumption> {
    let mut wanted = target.amount.abs();
    let mut consumed = Vec::new();
    for (candidate, remaining) in left.iter_mut() {
        let repays = candidate.currency == target.currency
            && !remaining.is_zero()
            && remaining.is_sign_negative() != target.amount.is_sign_negative();
        if !repays || wanted.is_zero() {
            continue;
        }
        let most = wanted.min(remaining.abs());
        let take = match remaining.is_sign_negative() {
            true => -most,
            false => most,
        };
        consumed.push(Consumption { source: candidate.ref_id, amount: take });
        *remaining -= take;
        wanted -= most;
    }
    consumed
}

/// A repayment and the splits it could settle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub repayment: Unallocated,
    /// Split and how much of the repayment goes there, signed as
    /// `bill_split::Repayment::amount` takes it.
    pub settles: Vec<(i64, Decimal)>,
    /// What would stay on the account, unallocated.
    pub residual: Decimal,
}

/// Suggestions for one person, over all their accounts.
pub async fn suggest(pool: &SqlitePool, books: &Books, person: i64) -> Result<Vec<Suggestion>> {
    let repayments = unallocated(pool, books, person).await?;
    let open: Vec<OpenShare> = open_for(pool, person).await?;
    let record = |ref_id: usize, date, amount, currency: ledger_types::currency::Currency| Record {
        ref_id: ref_id as i64,
        date,
        amount,
        currency: currency.to_string(),
    };
    let targets =
        repayments.iter().enumerate().map(|(i, r)| record(i, r.date, r.left, r.currency)).collect();
    let candidates =
        open.iter().enumerate().map(|(i, o)| record(i, o.since, o.owed, o.currency)).collect();
    let outcome = Engine::new(vec![Box::new(SplitRepaymentMode)]).run(targets, candidates);
    Ok(outcome
        .matches
        .into_iter()
        .map(|m| Suggestion {
            repayment: repayments[m.target as usize].clone(),
            settles: m
                .consumed
                .iter()
                .map(|c| (open[c.source as usize].split_id, -c.amount))
                .collect(),
            residual: m.residual,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn rec(ref_id: i64, date: &str, amount: Decimal) -> Record {
        Record { ref_id, date: date.parse().unwrap(), amount, currency: "TWD".into() }
    }

    fn run(targets: Vec<Record>, candidates: Vec<Record>) -> crate::matcher::Outcome {
        Engine::new(vec![Box::new(SplitRepaymentMode)]).run(targets, candidates)
    }

    #[test]
    fn an_exact_amount_wins_over_an_older_debt() {
        let outcome = run(vec![rec(1, "2026-06-01", dec!(-300))], vec![
            rec(10, "2026-01-01", dec!(500)),
            rec(11, "2026-05-01", dec!(300)),
        ]);
        assert_eq!(outcome.rejected, vec![]);
        assert_eq!(outcome.matches[0].sources(), vec![11]);
        assert_eq!(outcome.matches[0].residual, dec!(0));
    }

    #[test]
    fn an_exact_match_is_not_stolen_by_an_earlier_partial_one() {
        let outcome =
            run(vec![rec(1, "2026-06-01", dec!(-1000)), rec(2, "2026-06-02", dec!(-300))], vec![
                rec(10, "2026-01-01", dec!(800)),
                rec(11, "2026-02-01", dec!(300)),
            ]);
        assert_eq!(outcome.rejected, vec![]);
        let of = |target: i64| outcome.matches.iter().find(|m| m.target == target).unwrap();
        assert_eq!(of(1).sources(), vec![10]);
        assert_eq!(of(1).residual, dec!(-200));
        assert_eq!(of(2).sources(), vec![11]);
    }

    #[test]
    fn a_repayment_runs_through_the_oldest_debts_and_may_stop_partway() {
        let outcome = run(vec![rec(1, "2026-06-01", dec!(-700))], vec![
            rec(11, "2026-03-01", dec!(500)),
            rec(10, "2026-01-01", dec!(400)),
        ]);
        assert_eq!(outcome.rejected, vec![]);
        assert_eq!(outcome.matches[0].consumed, vec![
            Consumption { source: 10, amount: dec!(400) },
            Consumption { source: 11, amount: dec!(300) },
        ]);
        assert_eq!(outcome.matches[0].residual, dec!(0));
    }

    #[test]
    fn a_repayment_may_be_months_older_than_the_debt() {
        let outcome =
            run(vec![rec(1, "2025-01-01", dec!(-300))], vec![rec(10, "2026-06-01", dec!(300))]);
        assert_eq!(outcome.matches[0].sources(), vec![10]);
    }

    #[test]
    fn it_never_crosses_a_sign_or_a_currency() {
        let usd = Record { currency: "USD".into(), ..rec(11, "2026-01-01", dec!(300)) };
        let outcome =
            run(vec![rec(1, "2026-06-01", dec!(300)), rec(2, "2026-06-01", dec!(-300))], vec![
                rec(10, "2026-01-01", dec!(300)),
                usd,
            ]);
        assert_eq!(outcome.rejected, vec![]);
        assert_eq!(outcome.unmatched, vec![1]);
        assert_eq!(outcome.matches[0].sources(), vec![10]);
    }

    #[test]
    fn where_they_fronted_it_your_payment_is_the_repayment() {
        let outcome =
            run(vec![rec(1, "2026-06-01", dec!(200))], vec![rec(10, "2026-01-01", dec!(-500))]);
        assert_eq!(outcome.rejected, vec![]);
        assert_eq!(outcome.matches[0].consumed, vec![Consumption {
            source: 10,
            amount: dec!(-200)
        }]);
        assert_eq!(outcome.matches[0].residual, dec!(0));
    }
}
