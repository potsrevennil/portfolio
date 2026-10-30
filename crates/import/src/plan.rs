//! Decides what a bank import writes, without touching the database: which
//! statement lines the ledger already holds, how the new ones pair up, and
//! whether the result chains onto the ledger's balances day by day.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
};

use anyhow::{bail, Result};
use chrono::NaiveDate;
use db::{
    import::{Origin, Posting, Transaction},
    import_batch::{LedgerPosting, LegPart, LegSplit, Stamp, Verification, Vouched},
    pairing::Ambiguity,
};
use ledger::{
    accounts::Chart,
    model::{Source, CONVERSIONS, OPENING_EQUITY},
    names::fallback_account,
    statements::{
        bank::{Bank, BankStatement, StatementLine},
        transfer::Half,
    },
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

/// Lines that pair with unverified records only by a choice nobody has made
/// yet. Nothing is imported; the caller records them for a person to pick.
#[derive(Debug, PartialEq, Eq)]
pub struct Ambiguous(pub Vec<Ambiguity>);

impl fmt::Display for Ambiguous {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lines and unverified records pair ambiguously; pick each line's record in the review \
             queue, then import again. Nothing was imported."
        )?;
        for a in &self.0 {
            write!(
                f,
                "\n  {} {}: {} {} matches {} unverified records within {WINDOW_DAYS} days",
                a.account,
                a.currency,
                a.date,
                a.amount,
                a.candidates.len()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for Ambiguous {}

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

impl Counts {
    fn count(&mut self, status: Status) {
        match status {
            Status::PredatesOpening => self.predate_opening += 1,
            Status::Known => self.known += 1,
            Status::Covered => self.covered += 1,
            Status::Verifies(_) | Status::Vouches => self.verified += 1,
            Status::New => self.new += 1,
        }
    }
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
    /// What statements that vouch for records say of them.
    pub stamps: Vec<Stamp>,
    /// Records whose one leg several of those statements' lines add up to.
    pub leg_splits: Vec<LegSplit>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    PredatesOpening,
    Known,
    Covered,
    /// Verifies the unverified record with this transaction id.
    Verifies(i64),
    /// Vouches for records already on the ledger, stamped in [`Plan::stamps`].
    Vouches,
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
    let mut ambiguous: Vec<Ambiguity> = Vec::new();
    let mut shared: Option<String> = None;

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
        //
        // A leg still tagged unverified is never one of those, whatever its
        // transaction stands for: the other bank's line vouched for the other
        // leg, and this one is here to be verified, not to cover a line.
        let mut spare: HashMap<(NaiveDate, Decimal), usize> = HashMap::new();
        for p in &s.existing {
            if !st.bank.vouches_for_records() && !p.opening && !p.unverified && from_a_statement(p)
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
        if st.bank.vouches_for_records() {
            let (stamps, leg_splits) = vouch(st, &s.existing, &refs[si], &mut line_status);
            plan.stamps.extend(stamps);
            plan.leg_splits.extend(leg_splits);
            for s in &line_status {
                plan.counts.count(*s);
            }
            status.push(line_status);
            continue;
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
        for (l, many) in &pairing.ambiguous {
            let li = new_lines[*l];
            let line = &st.lines[li];
            ambiguous.push(Ambiguity {
                account: s.account.clone(),
                currency: st.currency,
                statement_ref: refs[si][li].clone(),
                date: line.book_date,
                amount: line.delta(),
                description: line.description.clone(),
                candidates: many.iter().map(|&r| unverified[r].transaction_id).collect(),
            });
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
                && (settled - p.date).num_days() < WINDOW_DAYS;
            if !pending || verifying.contains(&p.transaction_id) {
                continue;
            }
            // Moving the record moves its every leg, so one reaching another
            // account that has its own statement is not ours to move.
            if let Some(other) = p.other_accounts.iter().find(|a| has_statements.contains(&***a)) {
                shared.get_or_insert_with(|| {
                    format!(
                        "{} {}: the record of {} on {} is also {other}'s, and this statement does \
                         not show it. Import it together with the next statement, which may, or \
                         review the record. Nothing was imported",
                        s.account, st.currency, p.amount, p.date
                    )
                });
                continue;
            }
            plan.deferred.push(Deferred { transaction_id: p.transaction_id, date: after });
            plan.counts.deferred += 1;
        }
        for s in &line_status {
            plan.counts.count(*s);
        }
        status.push(line_status);
    }

    // Ambiguous lines first: they are what a person can settle in 待配對,
    // and the record the other refusal names may be one of their candidates.
    if !ambiguous.is_empty() {
        return Err(Ambiguous(ambiguous).into());
    }
    if let Some(shared) = shared {
        bail!(shared);
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
    let half = |at: (usize, usize)| Half { statement: statements[at.0].statement, line: line(at) };
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
        let open =
            new.iter().copied().filter(|&other| !shape.contains_key(&other) && is_internal(other));
        let partner = half(at).partner(open.map(|other| (other, half(other))));
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
            dates: line(at).book_date.into(),
            amount: line(at).delta(),
            currency: statements[at.0].statement.currency.to_string(),
        })
        .collect();
    let pool: Vec<Record> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| Record {
            ref_id: i as i64,
            dates: c.date.into(),
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
        let own = match s.statement.bank.vouches_for_records() {
            true => own.posted_on(l.book_date),
            false => own,
        };
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
        let date = match &shape[&at] {
            Shape::Transfer(other) => half(at).booked_with(&half(*other)),
            _ => l.purchased.unwrap_or(l.book_date),
        };
        plan.transactions.push((s.files[li], Transaction {
            date,
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
    let stamped: HashMap<i64, Vec<(NaiveDate, Decimal)>> = plan
        .leg_splits
        .iter()
        .map(|s| (s.posting_id, s.parts.iter().map(|p| (p.posted, p.amount)).collect()))
        .chain(plan.stamps.iter().map(|s| (s.posting_id, vec![(s.posted, s.amount)])))
        .collect();
    chain(statements, &openings, &plan.transactions, &redated, &stamped)?;
    plan.openings = openings;
    Ok(plan)
}

/// A statement over records the ledger already holds: each new line finds the
/// records it stands for through the matcher, and its leg takes the line's
/// date. Several lines one record covers (a purchase and its foreign fee,
/// posted apart) split its leg. Lines it finds none for stay new. A record no
/// line shows that could still be to post waits for the next statement, or the
/// one missing after its period; an older one is left where it is, and the
/// balance chain refuses it.
fn vouch(
    st: &BankStatement,
    existing: &[LedgerPosting],
    refs: &[String],
    line_status: &mut [Status],
) -> (Vec<Stamp>, Vec<LegSplit>) {
    let own = st.bank.ref_prefix();
    let records: Vec<&LedgerPosting> = existing
        .iter()
        .filter(|p| {
            !p.opening
                && (p.unverified || p.posted.is_none())
                && !p.refs.iter().any(|r| r.starts_with(own))
        })
        .collect();
    let currency = st.currency.to_string();
    let as_line = |li: usize| Record {
        ref_id: li as i64,
        dates: st.lines[li].dates(),
        amount: st.lines[li].delta(),
        currency: currency.clone(),
    };
    let as_record = |i: usize, p: &LedgerPosting| Record {
        ref_id: i as i64,
        dates: p.date.into(),
        amount: p.amount,
        currency: currency.clone(),
    };
    let new_lines = |status: &[Status]| -> Vec<usize> {
        (0..st.lines.len()).filter(|&li| status[li] == Status::New).collect()
    };
    let lines = new_lines(line_status).into_iter().map(as_line).collect();
    let pool = records.iter().enumerate().map(|(i, p)| as_record(i, p)).collect();
    let outcome = Engine::with_default_modes().run(lines, pool);

    let mut stamps = Vec::new();
    let mut shown: HashSet<i64> = HashSet::new();
    for m in &outcome.matches {
        let li = m.target as usize;
        line_status[li] = Status::Vouches;
        for (n, c) in m.consumed.iter().enumerate() {
            let p = records[c.source as usize];
            shown.insert(p.id);
            stamps.push(Stamp {
                posting_id: p.id,
                transaction_id: p.transaction_id,
                posted: st.lines[li].book_date,
                amount: p.amount,
                vouched: Vouched::ByLine((n == 0).then(|| refs[li].clone())),
            });
        }
    }

    let left: Vec<usize> =
        (0..records.len()).filter(|&i| !shown.contains(&records[i].id)).collect();
    let targets = left.iter().map(|&i| as_record(i, records[i])).collect();
    let pool = new_lines(line_status).into_iter().map(as_line).collect();
    let mut leg_splits = Vec::new();
    for m in Engine::with_default_modes().run(targets, pool).matches {
        let p = records[m.target as usize];
        shown.insert(p.id);
        let mut parts = Vec::with_capacity(m.consumed.len());
        for c in &m.consumed {
            let li = c.source as usize;
            line_status[li] = Status::Vouches;
            parts.push(LegPart {
                amount: c.amount,
                posted: st.lines[li].book_date,
                line: refs[li].clone(),
            });
        }
        leg_splits.push(LegSplit { posting_id: p.id, transaction_id: p.transaction_id, parts });
    }

    // The days a line could still have come after: this import's last, and the
    // last before any statement missing between its periods.
    let breaks: Vec<NaiveDate> = st
        .periods
        .windows(2)
        .filter(|pair| pair[0].end.succ_opt() != Some(pair[1].start))
        .map(|pair| pair[0].end)
        .chain([st.settled_through()])
        .collect();
    for p in records.iter().filter(|p| !shown.contains(&p.id)) {
        let waits_past = breaks
            .iter()
            .find(|&&end| p.date <= end && (end - p.date).num_days() < st.bank.posts_within_days());
        if let Some(end) = waits_past {
            stamps.push(Stamp {
                posting_id: p.id,
                transaction_id: p.transaction_id,
                posted: end.succ_opt().expect("a day after a statement"),
                amount: p.amount,
                vouched: Vouched::NotYet,
            });
        }
    }
    (stamps, leg_splits)
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
    stamped: &HashMap<i64, Vec<(NaiveDate, Decimal)>>,
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
            .flat_map(|p| match stamped.get(&p.id) {
                Some(parts) => parts.clone(),
                None => {
                    let day = redated.get(&p.transaction_id).copied().unwrap_or(p.date);
                    vec![(p.posted.unwrap_or(day), p.amount)]
                }
            })
            .collect();
        let last = s.statement.lines.last().expect("load rejects empty statements").book_date;
        let mut adds_to_last_day = false;
        for (_, t) in planned {
            for p in &t.postings {
                if p.account == s.account && p.currency == currency {
                    let day = p.posted_date.unwrap_or(t.date);
                    movements.push((day, p.amount));
                    adds_to_last_day |= day == last;
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
