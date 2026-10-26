//! Decides what a bank import writes, without touching the database: which
//! statement lines the ledger already holds, how the new ones pair up, and
//! whether the result chains onto the ledger's balances day by day.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use chrono::NaiveDate;
use db::{
    import::{Origin, Posting, Transaction},
    import_batch::{LedgerPosting, Verification},
    pairing::Ambiguity,
    stops::{Refused, Stop},
};
use ledger::{
    accounts::Chart,
    model::{Source, CONVERSIONS, OPENING_EQUITY},
    names::fallback_account,
    statements::bank::{Bank, BankStatement, StatementLine},
};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use crate::{
    matcher::{Engine, Record},
    pairing::{one_to_one, within_window, WINDOW_DAYS},
};

/// One merged statement and where it lands.
pub struct Statement<'a> {
    pub account: String,
    pub statement: &'a BankStatement,
    /// Index of the file each line came from.
    pub files: Vec<usize>,
    /// What the ledger holds on `account` in the statement's currency.
    pub existing: Vec<LedgerPosting>,
    /// The record a person picked for a line, by the line's ref, where the
    /// line alone fit more than one.
    pub chosen: HashMap<String, i64>,
}

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
    /// Unverified records moved to the sibling account whose line is theirs.
    pub moved: usize,
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
    pub moved: Vec<Moved>,
}

/// An unverified record booked on one pooled account that a sibling's line
/// shows: its leg moves there, and that line verifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    pub transaction_id: i64,
    pub from: String,
    pub to: String,
    pub amount: Decimal,
    pub date: NaiveDate,
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
    let fallback_origin =
        |description: &str| match chart.fallback.descriptions.contains_key(description) {
            true => Origin::Rule,
            false => Origin::Fallback,
        };
    // Everything that stops the import, found in one pass so the queue can
    // show it all at once. Nothing is written while any is left.
    let mut stops: Vec<Stop> = Vec::new();
    // Unverified records no line of their own statement shows, by statement.
    let mut missing: Vec<(usize, &LedgerPosting)> = Vec::new();

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
            _ => {
                let stop =
                    Stop::SeveralOpenings { account: s.account.clone(), currency: st.currency };
                return Err(Refused(vec![stop]).into());
            }
        };
        openings.push(opening);

        // Postings the ledger booked from other lines (a transfer's far half, a
        // reversal) can each cover one line with no ref of its own. A known
        // line takes back its own posting first.
        //
        // A leg still tagged unverified is never one of those, whatever its
        // transaction stands for: the other bank's line vouched for the other
        // leg, and this one is here to be verified, not to cover a line.
        let mut spare: HashMap<(NaiveDate, Decimal), usize> = HashMap::new();
        for p in &s.existing {
            if !p.opening && !p.unverified && from_a_statement(p) {
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
        // with the records still unverified, until a person picks. A pick
        // takes its line and record out of the running for the rest; one
        // whose record no longer fits the line, edited since, is ignored.
        let unverified: Vec<&LedgerPosting> =
            s.existing.iter().filter(|p| p.unverified && !p.opening).collect();
        let fits = |line: &StatementLine, p: &LedgerPosting| {
            p.amount == line.delta() && within_window(p.date, line.book_date)
        };
        let mut picked: HashSet<i64> = HashSet::new();
        for li in 0..st.lines.len() {
            if line_status[li] != Status::New {
                continue;
            }
            let pick = s.chosen.get(&refs[si][li]).copied().filter(|id| {
                unverified.iter().any(|p| p.transaction_id == *id && fits(&st.lines[li], p))
                    && !picked.contains(id)
            });
            if let Some(id) = pick {
                picked.insert(id);
                line_status[li] = Status::Verifies(id);
            }
        }
        let unverified: Vec<&LedgerPosting> =
            unverified.into_iter().filter(|p| !picked.contains(&p.transaction_id)).collect();
        let new_lines: Vec<usize> =
            (0..st.lines.len()).filter(|&li| line_status[li] == Status::New).collect();
        let pairing = one_to_one(&new_lines, &unverified, |&li: &usize, p: &&LedgerPosting| {
            fits(&st.lines[li], p)
        });
        for &(l, r) in &pairing.pairs {
            line_status[new_lines[l]] = Status::Verifies(unverified[r].transaction_id);
        }
        let mut contested: HashSet<i64> = HashSet::new();
        for (l, many) in &pairing.ambiguous {
            let li = new_lines[*l];
            let line = &st.lines[li];
            contested.extend(many.iter().map(|&r| unverified[r].transaction_id));
            stops.push(Stop::LineAmbiguous(Ambiguity {
                account: s.account.clone(),
                currency: st.currency,
                statement_ref: refs[si][li].clone(),
                date: line.book_date,
                amount: line.delta(),
                description: line.description.clone(),
                candidates: many.iter().map(|&r| unverified[r].transaction_id).collect(),
            }));
        }
        // A record within the window of the end may be booked on the next
        // statement; an earlier one is not on it.
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
            // One a line may still be, once a person picks, is not missing.
            let shown = p.date <= opening
                || p.date > settled
                || verifying.contains(&p.transaction_id)
                || contested.contains(&p.transaction_id);
            if shown {
                continue;
            }
            if (settled - p.date).num_days() >= WINDOW_DAYS {
                missing.push((si, *p));
                continue;
            }
            // Moving the record moves its every leg, so one reaching another
            // account that has its own statement is not ours to move.
            if let Some(other) = p.other_accounts.iter().find(|a| has_statements.contains(&***a)) {
                stops.push(Stop::SharedDeferral {
                    transaction_id: p.transaction_id,
                    account: s.account.clone(),
                    currency: st.currency,
                    date: p.date,
                    amount: p.amount,
                    other: other.clone(),
                });
                continue;
            }
            plan.deferred.push(Deferred { transaction_id: p.transaction_id, date: after });
            plan.counts.deferred += 1;
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

    // A record its statement passes by may be a sibling's: the app records the
    // pooled accounts only together. With exactly one new sibling line of its
    // amount, which no other such record fits, it moves there and that line
    // verifies it; otherwise the import stops, naming the siblings that fit.
    let fitting: Vec<Vec<(usize, usize)>> = missing
        .iter()
        .map(|(si, p)| {
            let s = &statements[*si];
            let pool = chart.pool_siblings(&s.account);
            let mut at = Vec::new();
            for (o, other) in statements.iter().enumerate() {
                if !pool.contains(&other.account)
                    || other.statement.currency != s.statement.currency
                {
                    continue;
                }
                for (li, l) in other.statement.lines.iter().enumerate() {
                    if status[o][li] == Status::New
                        && l.delta() == p.amount
                        && within_window(l.book_date, p.date)
                    {
                        at.push((o, li));
                    }
                }
            }
            at
        })
        .collect();
    let claims = |line: &(usize, usize)| fitting.iter().filter(|f| f.contains(line)).count();
    for ((si, p), fits) in missing.iter().zip(&fitting) {
        let s = &statements[*si];
        match fits.as_slice() {
            [only] if claims(only) == 1 => {
                status[only.0][only.1] = Status::Verifies(p.transaction_id);
                plan.counts.new -= 1;
                plan.counts.verified += 1;
                plan.counts.moved += 1;
                plan.moved.push(Moved {
                    transaction_id: p.transaction_id,
                    from: s.account.clone(),
                    to: statements[only.0].account.clone(),
                    amount: p.amount,
                    date: p.date,
                });
            }
            _ => {
                let mut siblings: Vec<String> =
                    fits.iter().map(|(o, _)| statements[*o].account.clone()).collect();
                siblings.dedup();
                stops.push(Stop::NotOnStatement {
                    transaction_id: p.transaction_id,
                    account: s.account.clone(),
                    currency: s.statement.currency,
                    date: p.date,
                    amount: p.amount,
                    through: s.statement.settled_through(),
                    siblings,
                });
            }
        }
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
                    let leg: Posting = (&candidates[*ci].account, -*amount, currency).into();
                    ps.push(leg.with_origin(Origin::Tiantian));
                }
                let residual: Decimal = ps.iter().map(|p| p.amount).sum();
                if !residual.is_zero() {
                    let fallback = fallback_account(chart, &l.description, residual);
                    let leg: Posting = (&**fallback, -residual, currency).into();
                    ps.push(leg.with_origin(fallback_origin(&l.description)));
                }
                ps
            }
            Shape::Fallback => {
                plan.counts.uncategorised += 1;
                let fallback = fallback_account(chart, &l.description, l.delta());
                let leg: Posting = (&**fallback, -l.delta(), currency).into();
                vec![own, leg.with_origin(fallback_origin(&l.description))]
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
    let mut dated: Vec<(i64, &str, NaiveDate)> = Vec::new();
    for (si, s) in statements.iter().enumerate() {
        for (li, l) in s.statement.lines.iter().enumerate() {
            if let Status::Verifies(transaction_id) = status[si][li] {
                // A record moved here from a sibling is that sibling's.
                let record = s
                    .existing
                    .iter()
                    .chain(statements.iter().flat_map(|o| &o.existing))
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
                // A record already standing for another bank's line is on
                // that bank's date. Re-dating moves its every leg, and the two
                // banks may have booked it on different days, so the date the
                // other statement is checked against stands.
                let date = (!from_a_statement(record)).then_some(l.book_date);
                if let Some(date) = date {
                    dated.push((transaction_id, &s.account, date));
                }
                plan.verified.push(Verification {
                    transaction_id,
                    date,
                    statement_ref: refs[si][li].clone(),
                    unchecked,
                });
            }
        }
    }
    if !stops.is_empty() {
        return Err(Refused(stops).into());
    }

    let mut redated: HashMap<i64, NaiveDate> = plan
        .verified
        .iter()
        .filter_map(|v| v.date.map(|date| (v.transaction_id, date)))
        .chain(plan.deferred.iter().map(|d| (d.transaction_id, d.date)))
        .collect();
    // One record, one date: two banks may book it on different days (a
    // transfer near midnight). It keeps whichever of their dates every
    // statement it is on agrees with; only when none does is it a stop.
    let mut dates: BTreeMap<i64, Vec<(String, NaiveDate)>> = BTreeMap::new();
    for (transaction_id, account, date) in dated {
        dates.entry(transaction_id).or_default().push((account.to_string(), date));
    }
    let on =
        |s: &&Statement, dates: &[(String, NaiveDate)]| dates.iter().any(|(a, _)| *a == s.account);
    for (transaction_id, dates) in dates {
        if dates.iter().all(|(_, d)| *d == dates[0].1) {
            continue;
        }
        let mut days: Vec<NaiveDate> = dates.iter().map(|(_, d)| *d).collect();
        days.sort();
        days.dedup();
        let kept = days.into_iter().find(|&day| {
            redated.insert(transaction_id, day);
            let these =
                statements.iter().zip(openings.iter().copied()).filter(|(s, _)| on(s, &dates));
            chain(these, &plan.transactions, &plan.moved, &redated).is_none()
        });
        match kept {
            Some(day) => {
                redated.insert(transaction_id, day);
                for v in plan.verified.iter_mut().filter(|v| v.transaction_id == transaction_id) {
                    v.date = v.date.map(|_| day);
                }
            }
            None => stops.push(Stop::TwoDates { transaction_id, dates }),
        }
    }
    if !stops.is_empty() {
        return Err(Refused(stops).into());
    }

    if let Some(stop) = chain(
        statements.iter().zip(openings.iter().copied()),
        &plan.transactions,
        &plan.moved,
        &redated,
    ) {
        return Err(Refused(vec![stop]).into());
    }
    plan.openings = openings;
    Ok(plan)
}

/// The first statement day after the opening that the ledger plus this plan
/// does not end on the statement's own running balance.
///
/// This is what makes the `:n` repeat suffix safe: a download that starts
/// mid-day numbers its repeats from the wrong line, so a line is taken for one
/// already held (or the reverse) and the day ends off by that amount. A missing
/// or unevenly overlapping download shows up the same way.
fn chain<'a>(
    statements: impl IntoIterator<Item = (&'a Statement<'a>, NaiveDate)>,
    planned: &[(usize, Transaction)],
    moved: &[Moved],
    redated: &HashMap<i64, NaiveDate>,
) -> Option<Stop> {
    for (s, opening) in statements {
        let currency = s.statement.currency;
        let mut expected: BTreeMap<NaiveDate, Decimal> = BTreeMap::new();
        for l in s.statement.lines.iter().filter(|l| l.book_date > opening) {
            expected.insert(l.book_date, l.balance);
        }
        let on_day = |id: i64, date: NaiveDate| redated.get(&id).copied().unwrap_or(date);
        let moved_off = |p: &LedgerPosting| {
            moved.iter().any(|m| m.transaction_id == p.transaction_id && m.from == s.account)
        };
        let mut movements: Vec<(NaiveDate, Decimal)> = s
            .existing
            .iter()
            .filter(|p| !moved_off(p))
            .map(|p| (on_day(p.transaction_id, p.date), p.amount))
            .chain(
                moved
                    .iter()
                    .filter(|m| m.to == s.account)
                    .map(|m| (on_day(m.transaction_id, m.date), m.amount)),
            )
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
                return Some(Stop::Unchained {
                    account: s.account.clone(),
                    currency,
                    day,
                    ledger: balance,
                    statement: want,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
