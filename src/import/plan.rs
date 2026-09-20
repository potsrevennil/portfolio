//! Decides what a bank import writes, without touching the database: which
//! statement lines the ledger already holds, how the new ones pair up, and
//! whether the result chains onto the ledger's balances day by day.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::{
    currency::Currency,
    ledger::{
        accounts::Chart,
        model::{CONVERSIONS, OPENING_EQUITY},
        names::fallback_account,
        statements::cathay::{self, info_names_account, BankStatement},
    },
    matcher::{Engine, Record},
    store::import_batch::LedgerPosting,
};

/// One merged statement and where it lands.
pub struct Statement<'a> {
    pub account: String,
    pub statement: &'a BankStatement,
    /// Index of the file each line came from.
    pub files: Vec<usize>,
    /// What the ledger holds on `account` in the statement's currency.
    pub existing: Vec<LedgerPosting>,
}

/// A 天天記帳 record a line may be matched to, already resolved to the
/// account it books against.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub date: NaiveDate,
    pub amount: Decimal,
    pub currency: Currency,
    pub account: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    pub account: String,
    pub amount: Decimal,
    pub currency: Currency,
}

#[derive(Debug)]
pub struct Planned {
    pub date: NaiveDate,
    pub payee: String,
    pub narration: String,
    pub external_ref: String,
    pub file: usize,
    pub postings: Vec<Posting>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Folded into the account's opening.
    pub predate_opening: usize,
    pub known: usize,
    /// The far half of a transfer, or a reversal, the ledger already booked
    /// with its partner line.
    pub covered: usize,
    pub new: usize,
    pub openings: usize,
    pub matched: usize,
    pub uncategorised: usize,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub transactions: Vec<Planned>,
    pub counts: Counts,
    /// Each statement's opening date; the ledger tracks its lines after it.
    pub openings: Vec<NaiveDate>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    PredatesOpening,
    Known,
    Covered,
    New,
}

/// How a new line's other side is booked.
enum Shape {
    /// Emitted by its partner's transaction.
    Absorbed,
    Transfer((usize, usize)),
    Reversal(usize),
    InTransit,
    Matched(Vec<(usize, Decimal)>),
    Fallback,
}

pub fn plan(
    chart: &Chart,
    statements: &[Statement],
    known_refs: &HashSet<String>,
    candidates: &[Candidate],
) -> Result<Plan> {
    let mut plan = Plan::default();
    let refs: Vec<Vec<String>> = statements.iter().map(|s| s.statement.dedup_refs()).collect();

    // --- what the ledger already holds ---
    let mut openings: Vec<NaiveDate> = Vec::with_capacity(statements.len());
    let mut status: Vec<Vec<Status>> = Vec::with_capacity(statements.len());
    for (si, s) in statements.iter().enumerate() {
        let st = s.statement;
        let first = st.lines.first().expect("load rejects empty statements");
        let mut opening_dates: Vec<NaiveDate> =
            s.existing.iter().filter(|p| p.opening).map(|p| p.date).collect();
        opening_dates.dedup();
        let opening = match (opening_dates.as_slice(), s.existing.is_empty()) {
            ([], true) => {
                let date = first.book_date.pred_opt().unwrap_or(first.book_date);
                plan.transactions.push(Planned {
                    date,
                    payee: "Opening balance".to_string(),
                    narration: st.account_kind.clone(),
                    external_ref: cathay::opening_ref(&s.account, st.currency),
                    file: s.files[0],
                    postings: vec![
                        Posting {
                            account: s.account.clone(),
                            amount: st.opening_balance(),
                            currency: st.currency,
                        },
                        Posting {
                            account: OPENING_EQUITY.to_string(),
                            amount: -st.opening_balance(),
                            currency: st.currency,
                        },
                    ],
                });
                plan.counts.openings += 1;
                date
            }
            ([], false) => bail!(
                "{} {} has postings but no opening; its balance can't be chained",
                s.account,
                st.currency
            ),
            ([date], _) => *date,
            _ => bail!("{} {} has more than one opening", s.account, st.currency),
        };
        openings.push(opening);

        // Postings the ledger booked from other lines (a transfer's far half, a
        // reversal) can each cover one line with no ref of its own. A known
        // line takes back its own posting first.
        let mut spare: HashMap<(NaiveDate, Decimal), usize> = HashMap::new();
        for p in &s.existing {
            if !p.opening
                && p.external_ref.as_deref().is_some_and(|r| r.starts_with(cathay::REF_PREFIX))
            {
                *spare.entry((p.date, p.amount)).or_default() += 1;
            }
        }
        let mut line_status: Vec<Status> = st
            .lines
            .iter()
            .enumerate()
            .map(|(li, l)| {
                if l.book_date <= opening {
                    Status::PredatesOpening
                } else if known_refs.contains(&refs[si][li]) {
                    Status::Known
                } else {
                    Status::New
                }
            })
            .collect();
        for (li, l) in st.lines.iter().enumerate() {
            if line_status[li] == Status::Known {
                if let Some(n) = spare.get_mut(&(l.book_date, l.delta())).filter(|n| **n > 0) {
                    *n -= 1;
                }
            }
        }
        for (li, l) in st.lines.iter().enumerate() {
            if line_status[li] == Status::New {
                if let Some(n) = spare.get_mut(&(l.book_date, l.delta())).filter(|n| **n > 0) {
                    *n -= 1;
                    line_status[li] = Status::Covered;
                }
            }
        }
        for s in &line_status {
            match s {
                Status::PredatesOpening => plan.counts.predate_opening += 1,
                Status::Known => plan.counts.known += 1,
                Status::Covered => plan.counts.covered += 1,
                Status::New => plan.counts.new += 1,
            }
        }
        status.push(line_status);
    }

    // --- how the new lines pair up, as the freeze pairs them ---
    let new: Vec<(usize, usize)> = status
        .iter()
        .enumerate()
        .flat_map(|(si, ls)| {
            ls.iter().enumerate().filter(|(_, s)| **s == Status::New).map(move |(li, _)| (si, li))
        })
        .collect();
    let line = |(si, li): (usize, usize)| &statements[si].statement.lines[li];
    let own_no = |si: usize| statements[si].statement.account_no.as_str();
    let is_internal = |at: (usize, usize)| {
        chart
            .institution
            .accounts
            .keys()
            .any(|no| no != own_no(at.0) && info_names_account(&line(at).info, no))
    };
    let names = |at: (usize, usize), si: usize| {
        info_names_account(&line(at).info, own_no(si))
            || info_names_account(&line(at).memo, own_no(si))
    };

    let mut shape: HashMap<(usize, usize), Shape> = HashMap::new();
    for (si, s) in statements.iter().enumerate() {
        for (debit, reversal) in s.statement.reversals() {
            if status[si][debit] == Status::New && status[si][reversal] == Status::New {
                shape.insert((si, debit), Shape::Reversal(reversal));
                shape.insert((si, reversal), Shape::Absorbed);
            }
        }
    }
    for &at in &new {
        if shape.contains_key(&at) || !line(at).delta().is_sign_negative() || !is_internal(at) {
            continue;
        }
        let partner = new.iter().copied().find(|&other| {
            other.0 != at.0
                && statements[other.0].statement.currency == statements[at.0].statement.currency
                && !shape.contains_key(&other)
                && is_internal(other)
                && line(other).book_date == line(at).book_date
                && line(other).delta() == -line(at).delta()
        });
        if let Some(other) = partner {
            shape.insert(at, Shape::Transfer(other));
            shape.insert(other, Shape::Absorbed);
        }
    }
    for &at in &new {
        if shape.contains_key(&at) || !line(at).delta().is_sign_negative() {
            continue;
        }
        let partners: Vec<(usize, usize)> = new
            .iter()
            .copied()
            .filter(|&other| {
                let (a, b) = (statements[at.0].statement, statements[other.0].statement);
                a.currency != b.currency
                    && a.account_no != b.account_no
                    && !shape.contains_key(&other)
                    && line(other).book_date == line(at).book_date
                    && line(other).delta().is_sign_positive()
                    && names(at, other.0)
                    && names(other, at.0)
            })
            .collect();
        // Amounts in two currencies can't be compared, so only an unambiguous
        // partner pairs.
        if let [other] = partners.as_slice() {
            shape.insert(at, Shape::Transfer(*other));
            shape.insert(*other, Shape::Absorbed);
        }
    }
    let mut targets: Vec<(usize, usize)> = Vec::new();
    for &at in &new {
        if !shape.contains_key(&at) {
            if is_internal(at) {
                shape.insert(at, Shape::InTransit);
            } else {
                targets.push(at);
            }
        }
    }

    // --- the rest go through the matcher ---
    let records: Vec<Record> = targets
        .iter()
        .enumerate()
        .map(|(i, &at)| Record {
            ref_id: i as i64,
            date: line(at).book_date,
            amount: line(at).delta(),
            currency: statements[at.0].statement.currency.to_string(),
        })
        .collect();
    let pool: Vec<Record> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| Record {
            ref_id: i as i64,
            date: c.date,
            amount: c.amount,
            currency: c.currency.to_string(),
        })
        .collect();
    let outcome = Engine::with_default_modes().run(records, pool);
    for m in outcome.matches {
        let consumed = m.consumed.iter().map(|c| (c.source as usize, c.amount)).collect();
        shape.insert(targets[m.target as usize], Shape::Matched(consumed));
    }
    for &at in &targets {
        shape.entry(at).or_insert(Shape::Fallback);
    }

    // --- the transactions ---
    for &at in &new {
        let (si, li) = at;
        let s = &statements[si];
        let l = line(at);
        let currency = s.statement.currency;
        let own = Posting { account: s.account.clone(), amount: l.delta(), currency };
        let postings = match &shape[&at] {
            Shape::Absorbed => continue,
            Shape::Reversal(r) => {
                vec![own, Posting {
                    account: s.account.clone(),
                    amount: s.statement.lines[*r].delta(),
                    currency,
                }]
            }
            Shape::Transfer(other) => {
                let far = &statements[other.0];
                let far_amount = line(*other).delta();
                let mut ps = vec![own, Posting {
                    account: far.account.clone(),
                    amount: far_amount,
                    currency: far.statement.currency,
                }];
                if far.statement.currency != currency {
                    ps.push(Posting {
                        account: CONVERSIONS.to_string(),
                        amount: -l.delta(),
                        currency,
                    });
                    ps.push(Posting {
                        account: CONVERSIONS.to_string(),
                        amount: -far_amount,
                        currency: far.statement.currency,
                    });
                }
                ps
            }
            Shape::InTransit => vec![own, Posting {
                account: chart.institution.clearing.to_string(),
                amount: -l.delta(),
                currency,
            }],
            Shape::Matched(consumed) => {
                plan.counts.matched += 1;
                let mut ps = vec![own];
                for (ci, amount) in consumed {
                    ps.push(Posting {
                        account: candidates[*ci].account.clone(),
                        amount: -*amount,
                        currency,
                    });
                }
                let residual: Decimal = ps.iter().map(|p| p.amount).sum();
                if !residual.is_zero() {
                    ps.push(Posting {
                        account: fallback_account(chart, &l.description, residual).to_string(),
                        amount: -residual,
                        currency,
                    });
                }
                ps
            }
            Shape::Fallback => {
                plan.counts.uncategorised += 1;
                vec![own, Posting {
                    account: fallback_account(chart, &l.description, l.delta()).to_string(),
                    amount: -l.delta(),
                    currency,
                }]
            }
        };
        let payee = match &shape[&at] {
            Shape::Reversal(r) => s.statement.lines[*r].description.clone(),
            _ => l.description.clone(),
        };
        let narration = [l.info.as_str(), l.memo.as_str()]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        plan.transactions.push(Planned {
            date: l.book_date,
            payee,
            narration,
            external_ref: refs[si][li].clone(),
            file: s.files[li],
            postings,
        });
    }

    chain(statements, &openings, &plan.transactions)?;
    plan.openings = openings;
    Ok(plan)
}

/// Fails unless, on every statement day after the opening, the ledger plus
/// this plan ends the day on the statement's own running balance.
///
/// This is what makes the `:n` repeat suffix safe: a download that starts
/// mid-day numbers its repeats from the wrong line, so a line is taken for one
/// already held (or the reverse) and the day ends off by that amount. A missing
/// or unevenly overlapping download shows up the same way.
fn chain(statements: &[Statement], openings: &[NaiveDate], planned: &[Planned]) -> Result<()> {
    for (s, &opening) in statements.iter().zip(openings) {
        let currency = s.statement.currency;
        let mut expected: BTreeMap<NaiveDate, Decimal> = BTreeMap::new();
        for l in s.statement.lines.iter().filter(|l| l.book_date > opening) {
            expected.insert(l.book_date, l.balance);
        }
        let mut movements: Vec<(NaiveDate, Decimal)> =
            s.existing.iter().map(|p| (p.date, p.amount)).collect();
        for t in planned {
            for p in &t.postings {
                if p.account == s.account && p.currency == currency {
                    movements.push((t.date, p.amount));
                }
            }
        }
        movements.sort_by_key(|(date, _)| *date);

        let mut balance = Decimal::ZERO;
        let mut next = movements.iter().peekable();
        for (&day, &want) in &expected {
            while let Some((_, amount)) = next.next_if(|(date, _)| *date <= day) {
                balance += amount;
            }
            if balance != want {
                bail!(
                    "{} {currency}: the ledger would end {day} at {balance} but the statement \
                     says {want}. A download starts mid-day, overlaps one already imported \
                     unevenly, or one is missing; nothing was imported",
                    s.account
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
