//! A bank line matched to the subset of 天天記帳 records summing to it, via
//! `ledger::matching::match_subsets`.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ledger::matching::match_subsets;
use rust_decimal::Decimal;

use super::{Consumption, Match, MatchMode, Pool, Record};

pub const MODE: &str = "statement-line";

#[derive(Default)]
pub struct StatementLineMode;

impl MatchMode for StatementLineMode {
    fn name(&self) -> &'static str { MODE }

    fn propose(&self, targets: &[Record], pool: &Pool) -> Vec<Match> {
        // The core matches on amount alone, so currencies must not share a pool.
        let mut candidates_by_ccy: BTreeMap<&str, Vec<(i64, NaiveDate, Decimal)>> = BTreeMap::new();
        for avail in pool.available() {
            candidates_by_ccy.entry(avail.record.currency.as_str()).or_default().push((
                avail.record.ref_id,
                avail.record.date,
                avail.remaining,
            ));
        }

        let mut targets_by_ccy: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
        for t in targets {
            targets_by_ccy.entry(t.currency.as_str()).or_default().push(t);
        }

        let mut matches = Vec::new();
        for (ccy, group) in targets_by_ccy {
            let candidates = match candidates_by_ccy.get(ccy) {
                None => continue,
                Some(c) => c,
            };

            let lines: Vec<(NaiveDate, Decimal)> =
                group.iter().map(|t| (t.date, t.amount)).collect();
            let events: Vec<(NaiveDate, Decimal)> =
                candidates.iter().map(|(_, date, amount)| (*date, *amount)).collect();

            for (li, assigned) in match_subsets(&lines, &events).into_iter().enumerate() {
                if let Some(subset) = assigned {
                    let consumed: Vec<Consumption> = subset
                        .iter()
                        .map(|ei| {
                            let (ref_id, _, amount) = candidates[*ei];
                            Consumption { source: ref_id, amount }
                        })
                        .collect();
                    let drawn: Decimal = consumed.iter().map(|c| c.amount).sum();
                    matches.push(Match {
                        mode: MODE,
                        target: group[li].ref_id,
                        consumed,
                        residual: group[li].amount - drawn,
                    });
                }
            }
        }
        matches
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    fn rec(ref_id: i64, date: &str, amount: Decimal, ccy: &str) -> Record {
        Record { ref_id, date: date.parse().unwrap(), amount, currency: ccy.into() }
    }

    #[test]
    fn matches_a_split_bank_line_to_two_records() {
        let targets = vec![rec(1, "2026-02-01", dec!(-12000), "TWD")];
        let pool = Pool::new(vec![
            rec(101, "2026-02-01", dec!(-10000), "TWD"),
            rec(102, "2026-02-01", dec!(-2000), "TWD"),
        ]);
        let matches = StatementLineMode.propose(&targets, &pool);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].residual, dec!(0));
        let mut s = matches[0].sources();
        s.sort();
        assert_eq!(s, vec![101, 102]);
    }

    #[test]
    fn does_not_cross_currencies_even_on_equal_amounts() {
        let targets = vec![rec(1, "2026-02-01", dec!(-100), "USD")];
        let pool = Pool::new(vec![rec(101, "2026-02-01", dec!(-100), "TWD")]);
        assert_eq!(StatementLineMode.propose(&targets, &pool), vec![]);
    }

    #[test]
    fn widens_the_date_window_across_passes() {
        let targets = vec![rec(1, "2026-02-03", dec!(-5000), "TWD")];
        let pool = Pool::new(vec![rec(101, "2026-02-01", dec!(-5000), "TWD")]);
        let matches = StatementLineMode.propose(&targets, &pool);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].sources(), vec![101]);
    }
}
