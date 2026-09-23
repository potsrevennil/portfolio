//! Decides what a bank import writes, without touching the database: which
//! statement lines the ledger already holds, how the new ones pair up, and
//! whether the result chains onto the ledger's balances day by day.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{bail, Result};
use chrono::NaiveDate;
use db::{
    import::{Posting, Transaction},
    import_batch::{LedgerPosting, Verification},
};
use ledger::{
    accounts::Chart,
    model::{Source, CONVERSIONS, OPENING_EQUITY},
    names::fallback_account,
    statements::bank::{Bank, BankStatement, StatementLine},
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::matcher::{Engine, Record};

/// One merged statement and where it lands.
pub struct Statement<'a> {
    pub account: String,
    pub statement: &'a BankStatement,
    /// Index of the file each line came from.
    pub files: Vec<usize>,
    /// What the ledger holds on `account` in the statement's currency.
    pub existing: Vec<LedgerPosting>,
}

/// How far a record's date may be from the line that verifies it: the
/// records take the bank's date when they are a week or more off.
const VERIFY_WINDOW_DAYS: i64 = 7;

/// An unverified record from a statement's last days that none of its lines
/// verifies: the bank books it after the statement, so it moves to the day
/// after, still unverified, for the next statement to verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    pub transaction_id: i64,
    pub date: NaiveDate,
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Folded into the account's opening.
    pub predate_opening: usize,
    pub known: usize,
    /// The far half of a transfer, or a reversal, the ledger already booked
    /// with its partner line.
    pub covered: usize,
    pub new: usize,
    /// Unverified records a line verified.
    pub verified: usize,
    /// Unverified records the statement ends before.
    pub deferred: usize,
    pub openings: usize,
    pub matched: usize,
    pub uncategorised: usize,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Each with the index of the file it came from; its batch is assigned at
    /// insert.
    pub transactions: Vec<(usize, Transaction)>,
    pub counts: Counts,
    /// Each statement's opening date; the ledger tracks its lines after it.
    pub openings: Vec<NaiveDate>,
    pub verified: Vec<Verification>,
    pub deferred: Vec<Deferred>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    PredatesOpening,
    Known,
    Covered,
    /// Verifies the unverified record with this transaction id.
    Verifies(i64),
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
    // Accounts a statement of their own vouches for; a leg on one of those is
    // only this import's business when its own statement is here.
    let has_statements: HashSet<&str> =
        chart.institution.accounts.values().map(|a| a.as_ref()).collect();
    let from_a_statement = |p: &LedgerPosting| {
        p.refs.iter().any(|r| Bank::ALL.iter().any(|bank| r.starts_with(bank.ref_prefix())))
    };

    // --- what the ledger already holds ---
    let mut openings: Vec<NaiveDate> = Vec::with_capacity(statements.len());
    let mut status: Vec<Vec<Status>> = Vec::with_capacity(statements.len());
    for (si, s) in statements.iter().enumerate() {
        let st = s.statement;
        let mut opening_dates: Vec<NaiveDate> =
            s.existing.iter().filter(|p| p.opening).map(|p| p.date).collect();
        opening_dates.dedup();
        let opening = match (opening_dates.as_slice(), s.existing.first()) {
            ([], None) => {
                let date = st.opening_date();
                plan.transactions.push((s.files[0], Transaction {
                    date,
                    payee: Some("Opening balance".to_string()),
                    narration: Some(st.account_kind.clone()),
                    source: Source::Import,
                    external_ref: Some(st.opening_ref(&s.account)),
                    import_batch_id: None,
                    postings: vec![
                        (&s.account, st.opening_balance(), st.currency).into(),
                        (OPENING_EQUITY, -st.opening_balance(), st.currency).into(),
                    ],
                }));
                plan.counts.openings += 1;
                date
            }
            // Records carry the account from its first posting; the balance
            // chain below proves they reach the statement's balances.
            ([], Some(earliest)) => earliest.date.pred_opt().expect("a day before a posting"),
            ([date], _) => *date,
            _ => bail!("{} {} has more than one opening", s.account, st.currency),
        };
        openings.push(opening);

        // Postings the ledger booked from other lines (a transfer's far half, a
        // reversal) can each cover one line with no ref of its own. A known
        // line takes back its own posting first.
        let mut spare: HashMap<(NaiveDate, Decimal), usize> = HashMap::new();
        for p in &s.existing {
            if !p.opening && from_a_statement(p) {
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
        // A new line verifies an unverified record of its amount within the
        // window, but only one-to-one: with two candidates either way (two
        // lunches of the same price) nothing chooses, and the import stops
        // with the records still unverified, for review.
        let unverified: Vec<&LedgerPosting> =
            s.existing.iter().filter(|p| p.unverified && !p.opening).collect();
        let fits = |l: &StatementLine, p: &LedgerPosting| {
            p.amount == l.delta() && (p.date - l.book_date).num_days().abs() <= VERIFY_WINDOW_DAYS
        };
        let new_lines: Vec<usize> =
            (0..st.lines.len()).filter(|&li| line_status[li] == Status::New).collect();
        let mut ambiguous: Vec<String> = Vec::new();
        for &li in &new_lines {
            let l = &st.lines[li];
            let records: Vec<&&LedgerPosting> = unverified.iter().filter(|p| fits(l, p)).collect();
            let rivals =
                |p: &LedgerPosting| new_lines.iter().filter(|&&o| fits(&st.lines[o], p)).count();
            match records.as_slice() {
                [] => {}
                [only] if rivals(only) == 1 => {
                    line_status[li] = Status::Verifies(only.transaction_id);
                }
                many => ambiguous.push(format!(
                    "{} {} matches {} unverified records within {VERIFY_WINDOW_DAYS} days ({})",
                    l.book_date,
                    l.delta(),
                    many.len(),
                    many.iter().map(|p| p.date.to_string()).collect::<Vec<_>>().join(", ")
                )),
            }
        }
        // A record within the window of the end may be booked on the next
        // statement; an earlier one still breaks the chain below.
        let settled = st.settled_through();
        let after = settled.succ_opt().expect("a day after a statement");
        let verifying: HashSet<i64> = line_status
            .iter()
            .filter_map(|s| match s {
                Status::Verifies(id) => Some(*id),
                _ => None,
            })
            .collect();
        for p in &unverified {
            let pending = p.date > opening
                && p.date <= settled
                && (settled - p.date).num_days() < VERIFY_WINDOW_DAYS;
            if !pending || verifying.contains(&p.transaction_id) {
                continue;
            }
            // Moving the record moves its every leg, so one reaching another
            // account that has its own statement is not ours to move.
            if let Some(other) = p.other_accounts.iter().find(|a| has_statements.contains(&***a)) {
                bail!(
                    "{} {}: the record of {} on {} is also {other}'s, and this statement does not \
                     show it. Import it together with the next statement, which may, or review \
                     the record. Nothing was imported",
                    s.account,
                    st.currency,
                    p.amount,
                    p.date
                );
            }
            plan.deferred.push(Deferred { transaction_id: p.transaction_id, date: after });
            plan.counts.deferred += 1;
        }
        if !ambiguous.is_empty() {
            bail!(
                "{} {}: lines and unverified records pair ambiguously; review them, then import \
                 again. Nothing was imported.\n  {}",
                s.account,
                st.currency,
                ambiguous.join("\n  ")
            );
        }
        for s in &line_status {
            match s {
                Status::PredatesOpening => plan.counts.predate_opening += 1,
                Status::Known => plan.counts.known += 1,
                Status::Covered => plan.counts.covered += 1,
                Status::Verifies(_) => plan.counts.verified += 1,
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
        let st = statements[at.0].statement;
        chart
            .institution
            .accounts
            .keys()
            .any(|no| no != own_no(at.0) && st.names_account(&line(at).info, no))
    };
    let names = |at: (usize, usize), si: usize| {
        let st = statements[at.0].statement;
        st.names_account(&line(at).info, own_no(si)) || st.names_account(&line(at).memo, own_no(si))
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
        let own: Posting = (&s.account, l.delta(), currency).into();
        let postings = match &shape[&at] {
            Shape::Absorbed => continue,
            Shape::Reversal(r) => {
                vec![own, (&s.account, s.statement.lines[*r].delta(), currency).into()]
            }
            Shape::Transfer(other) => {
                let far = &statements[other.0];
                let far_amount = line(*other).delta();
                let mut ps = vec![own, (&far.account, far_amount, far.statement.currency).into()];
                if far.statement.currency != currency {
                    ps.push((CONVERSIONS, -l.delta(), currency).into());
                    ps.push((CONVERSIONS, -far_amount, far.statement.currency).into());
                }
                ps
            }
            Shape::InTransit => {
                vec![own, (&*chart.institution.clearing, -l.delta(), currency).into()]
            }
            Shape::Matched(consumed) => {
                plan.counts.matched += 1;
                let mut ps = vec![own];
                for (ci, amount) in consumed {
                    ps.push((&candidates[*ci].account, -*amount, currency).into());
                }
                let residual: Decimal = ps.iter().map(|p| p.amount).sum();
                if !residual.is_zero() {
                    let fallback = fallback_account(chart, &l.description, residual);
                    ps.push((&**fallback, -residual, currency).into());
                }
                ps
            }
            Shape::Fallback => {
                plan.counts.uncategorised += 1;
                let fallback = fallback_account(chart, &l.description, l.delta());
                vec![own, (&**fallback, -l.delta(), currency).into()]
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
        plan.transactions.push((s.files[li], Transaction {
            date: l.book_date,
            payee: Some(payee),
            narration: (!narration.is_empty()).then_some(narration),
            source: Source::Import,
            external_ref: Some(refs[si][li].clone()),
            import_batch_id: None,
            postings,
        }));
    }

    // --- lines that verify an unverified record: it keeps its row ---
    //
    // A transfer between two accounts is one record with a leg on each. Each
    // bank's line vouches for its own leg, so a leg whose account has a
    // statement no line here covers stays unverified.
    let checked: HashMap<i64, HashSet<&str>> = status.iter().enumerate().fold(
        HashMap::new(),
        |mut checked: HashMap<i64, HashSet<&str>>, (si, ls)| {
            for id in ls.iter().filter_map(|s| match s {
                Status::Verifies(id) => Some(*id),
                _ => None,
            }) {
                checked.entry(id).or_default().insert(&statements[si].account);
            }
            checked
        },
    );
    for (si, s) in statements.iter().enumerate() {
        for (li, l) in s.statement.lines.iter().enumerate() {
            if let Status::Verifies(transaction_id) = status[si][li] {
                let record = s
                    .existing
                    .iter()
                    .find(|p| p.transaction_id == transaction_id)
                    .expect("the record a line verifies");
                let unchecked: Vec<String> = record
                    .other_accounts
                    .iter()
                    .filter(|a| {
                        has_statements.contains(&***a) && !checked[&transaction_id].contains(&***a)
                    })
                    .cloned()
                    .collect();
                plan.verified.push(Verification {
                    transaction_id,
                    // A record already standing for another bank's line is on
                    // that bank's date. Re-dating moves its every leg, and the
                    // two banks may have booked it on different days, so the
                    // date the other statement is checked against stands.
                    date: (!from_a_statement(record)).then_some(l.book_date),
                    statement_ref: refs[si][li].clone(),
                    unchecked,
                });
            }
        }
    }
    // One record cannot sit on two days: two banks that booked it differently
    // need it split, which only a re-freeze can do.
    let mut dates: HashMap<i64, NaiveDate> = HashMap::new();
    for v in plan.verified.iter().filter(|v| v.date.is_some()) {
        let date = v.date.expect("filtered to the lines that re-date");
        if let Some(&other) = dates.get(&v.transaction_id).filter(|&&d| d != date) {
            bail!(
                "the record {} verifies was booked {} by one bank and {other} by the other; \
                 re-freeze so each side stands on its own. Nothing was imported",
                v.statement_ref,
                date
            );
        }
        dates.insert(v.transaction_id, date);
    }

    let redated: HashMap<i64, NaiveDate> = plan
        .verified
        .iter()
        .filter_map(|v| v.date.map(|date| (v.transaction_id, date)))
        .chain(plan.deferred.iter().map(|d| (d.transaction_id, d.date)))
        .collect();
    chain(statements, &openings, &plan.transactions, &redated)?;
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
fn chain(
    statements: &[Statement],
    openings: &[NaiveDate],
    planned: &[(usize, Transaction)],
    redated: &HashMap<i64, NaiveDate>,
) -> Result<()> {
    for (s, &opening) in statements.iter().zip(openings) {
        let currency = s.statement.currency;
        let mut expected: BTreeMap<NaiveDate, Decimal> = BTreeMap::new();
        for l in s.statement.lines.iter().filter(|l| l.book_date > opening) {
            expected.insert(l.book_date, l.balance);
        }
        let mut movements: Vec<(NaiveDate, Decimal)> = s
            .existing
            .iter()
            .map(|p| (redated.get(&p.transaction_id).copied().unwrap_or(p.date), p.amount))
            .collect();
        let last = s.statement.lines.last().expect("load rejects empty statements").book_date;
        let mut adds_to_last_day = false;
        for (_, t) in planned {
            for p in &t.postings {
                if p.account == s.account && p.currency == currency {
                    movements.push((t.date, p.amount));
                    adds_to_last_day |= t.date == last;
                }
            }
        }
        movements.sort_by_key(|(date, _)| *date);
        // An unsettled last day that adds nothing (a stale partial download
        // re-imported) proves nothing: the ledger may hold the rest of it.
        let unsettled = last > s.statement.settled_through();
        let skip = (unsettled && !adds_to_last_day).then_some(last);

        let mut balance = Decimal::ZERO;
        let mut next = movements.iter().peekable();
        for (&day, &want) in &expected {
            while let Some((_, amount)) = next.next_if(|(date, _)| *date <= day) {
                balance += amount;
            }
            if Some(day) != skip && balance != want {
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
