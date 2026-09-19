//! Assembles the ledger: statements first, then everything 天天記帳 covers.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::Write as _,
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::{
    accounts::{self, AccountType},
    args::Args,
    corrected, daily,
    emit::{contra_posting, emit_daily_accounts, narration_for, resolve},
    matching,
    model::{self, Directive},
    names::{fallback_account, statement_account},
    statements,
    summary::Summary,
    writer,
};
use crate::currency::Currency;

/// Each account's balance as the app's own records imply it, per currency.
///
/// Computed straight from 天天記帳, with no knowledge of how the ledger emits
/// its postings — a 國泰 transfer's far side, a matched statement line, the
/// backfill, all reach the same account by different code paths above, and this
/// sums none of that, only the raw records. Asserting these values therefore
/// makes `bean-check` prove the emitted ledger matches the app, rather than
/// restating what the importer just wrote. Statement-driven app accounts are
/// asserted against their statements instead.
fn app_balances(
    entries: &[daily::Entry],
    chart: &accounts::Chart,
    statement_pools: &BTreeSet<&str>,
) -> BTreeMap<(String, Currency), Decimal> {
    let mut bal: BTreeMap<(String, Currency), Decimal> = BTreeMap::new();
    for entry in entries {
        let mut add = |name: &str, amount: Decimal, currency: Currency| {
            if statement_pools.contains(name) {
                return;
            }
            if let Some(mapping) = chart.account(name) {
                *bal.entry((mapping.account.to_string(), currency)).or_default() += amount;
            }
        };
        match entry {
            daily::Entry::Flow { account, amount, currency, .. } => {
                add(account, *amount, *currency);
            }
            daily::Entry::Transfer { from, out, out_currency, to, inn, in_currency, .. } => {
                add(from, -*out, *out_currency);
                add(to, *inn, *in_currency);
            }
        }
    }
    bal
}

/// A transfer between an account and the opening equity, as that account's
/// starting position: (ledger account, currency, signed amount, date).
fn opening(
    chart: &accounts::Chart,
    record: &daily::Entry,
) -> Result<Option<(String, Currency, Decimal, NaiveDate)>> {
    let is_equity =
        |name: &str| chart.account(name).is_some_and(|m| &*m.account == model::OPENING_EQUITY);
    match record {
        daily::Entry::Transfer { date, from, out, out_currency, to, inn, in_currency, .. }
            if is_equity(from) || is_equity(to) =>
        {
            let (label, amount, currency) = match is_equity(from) {
                true => (to, *inn, *in_currency),
                false => (from, -*out, *out_currency),
            };
            let account = chart.account(label).with_context(|| {
                format!("opening names {label:?}, which [accounts] does not map")
            })?;
            Ok(Some((account.account.to_string(), currency, amount, *date)))
        }
        _ => Ok(None),
    }
}

/// Assembles the ledger as an in-memory model: the transactions, balance
/// assertions and opens the reconciliation produces, with each transaction's
/// source and dedup id attached. [`build`] renders this to the generated
/// Beancount files; the freeze tool consumes it directly, so nothing
/// re-parses the importer's own output.
pub fn assemble(opts: &Args) -> Result<(model::Model, Summary)> {
    let ledger = opts.ledger_dir.as_path();

    let chart = accounts::Chart::load(ledger.join("mapping.toml"))?;
    // Accounts the build has to reach by role rather than by name, since the
    // name is the config's to choose.
    let savings: &str = &chart.institution.primary;
    let investment: &str = &chart.institution.settlement;
    let clearing: &str = &chart.institution.clearing;
    let settlement_source = chart.institution.settlement_app_account.clone();
    let institution_app: &str = &chart.institution.app_account;

    let records = match (
        opts.transactions.as_deref(),
        opts.daily_income_expense.as_deref(),
        opts.daily_transfers.as_deref(),
    ) {
        (Some(corrected), ..) => corrected::load(corrected)?,
        (None, Some(ie), Some(xf)) => daily::load_entries(ie, xf)?,
        _ => Vec::new(),
    };
    // Transfers with the opening equity are set apart: they are starting
    // positions, not movements a statement line could explain.
    let mut entries: Vec<daily::Entry> = Vec::new();
    let mut declared: Vec<(String, Currency, Decimal, NaiveDate)> = Vec::new();
    for record in records {
        match opening(&chart, &record)? {
            Some(position) => declared.push(position),
            None => entries.push(record),
        }
    }
    declared.sort();
    if let Some(pair) = declared.windows(2).find(|w| (&w[0].0, w[0].1) == (&w[1].0, w[1].1)) {
        anyhow::bail!("{} {} has more than one opening", pair[0].0, pair[0].1);
    }

    let mut merged = statements::cathay::load_merged(&opts.cathay_statements)?;

    // Lines before the records begin have nothing to match; fold them into
    // the opening balance.
    let records_start = entries.first().map(daily::Entry::date);
    let mut n_folded = 0usize;
    if let Some(start) = records_start {
        for m in &mut merged {
            n_folded += m.statement.trim_before(start);
            anyhow::ensure!(
                !m.statement.lines.is_empty(),
                "{} {}: every statement line predates the records ({start})",
                m.statement.account_no,
                m.statement.currency
            );
        }
    }

    // Ordered by the statement's earliest line rather than by argument order.
    // A per-account `.find()` (the derived-opening backfill below) must reach
    // the chronologically first statement for an account.
    merged.sort_by(|a, b| {
        let key = |s: &statements::cathay::BankStatement| {
            (s.lines.first().map(|l| l.book_date), s.account_no.clone(), s.currency)
        };
        key(&a.statement).cmp(&key(&b.statement))
    });
    let (statements, statement_paths): (Vec<statements::cathay::BankStatement>, Vec<_>) =
        merged.into_iter().map(|m| (m.statement, m.paths)).unzip();

    // 天天記帳 lumps the TWD Cathay accounts into one app account, so they share
    // a pool of records; an account the app keeps separately (外幣) gets its own.
    let pool_of: Vec<String> = statements
        .iter()
        .map(|s| -> Result<String> {
            let account = statement_account(&chart, &s.account_no)?;
            Ok(chart.app_account_for(account)?.unwrap_or(institution_app).to_string())
        })
        .collect::<Result<_>>()?;
    let pools: BTreeMap<&str, Vec<daily::AppEvent>> =
        pool_of.iter().map(|p| (p.as_str(), daily::view(&entries, p))).collect();
    let no_events: Vec<daily::AppEvent> = Vec::new();
    let app_events: &[daily::AppEvent] = pools.get(institution_app).unwrap_or(&no_events);

    // (statement index, line index) over every statement.
    let mut flat: Vec<(usize, usize)> = Vec::new();
    for (si, s) in statements.iter().enumerate() {
        for li in 0..s.lines.len() {
            flat.push((si, li));
        }
    }

    // Internal 活存 <-> 投資 transfers are settled against the clearing account and
    // withheld from matching, since 天天記帳 never recorded them.
    let is_internal = |si: usize, li: usize| -> bool {
        let info = &statements[si].lines[li].info;
        statements.iter().enumerate().any(|(other, s)| {
            other != si && statements::cathay::info_names_account(info, &s.account_no)
        })
    };

    // Both statements record the same internal movement, so pair the halves and
    // emit one transaction from the sending side. Nothing has to be recorded by
    // hand, and no clearing balance is left behind.
    //
    // Same-day only: if the two halves land on different days the money was
    // genuinely in transit, and routing that through the clearing account is the
    // honest description rather than back-dating one side to match the other.
    let internal: Vec<(usize, usize)> =
        flat.iter().copied().filter(|(si, li)| is_internal(*si, *li)).collect();
    let mut partner: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    let mut paired: HashSet<(usize, usize)> = HashSet::new();
    for &(si, li) in &internal {
        let line = &statements[si].lines[li];
        if !line.delta().is_sign_negative() || paired.contains(&(si, li)) {
            continue;
        }
        let counterpart = internal.iter().copied().find(|&(sj, lj)| {
            let other = &statements[sj].lines[lj];
            sj != si
                && statements[sj].currency == statements[si].currency
                && !paired.contains(&(sj, lj))
                && other.book_date == line.book_date
                && other.delta() == -line.delta()
        });
        if let Some(p) = counterpart {
            paired.insert((si, li));
            paired.insert(p);
            partner.insert((si, li), p);
        }
    }

    // Currency conversions: same day, opposite directions, each naming the
    // other (the TWD side only in 備註). The app's transfer record is reserved
    // on both sides so the matcher can't spend it on another line.
    let names = |line: &statements::cathay::StatementLine,
                 s: &statements::cathay::BankStatement| {
        statements::cathay::info_names_account(&line.info, &s.account_no)
            || statements::cathay::info_names_account(&line.memo, &s.account_no)
    };
    // The app's transfer record for one side of a conversion: same amounts
    // both ways, the other pool as contra, not yet reserved.
    let conversion_record = |near_si: usize,
                             near: &statements::cathay::StatementLine,
                             far_si: usize,
                             far: &statements::cathay::StatementLine,
                             taken: Option<&HashSet<usize>>|
     -> Option<usize> {
        let events = pools.get(pool_of[near_si].as_str()).unwrap_or(&no_events);
        events
            .iter()
            .enumerate()
            .filter(|(ei, e)| {
                !taken.is_some_and(|t| t.contains(ei))
                    && e.delta == near.delta()
                    && e.currency == statements[near_si].currency
                    && matches!(&e.contra, daily::Contra::Account(n) if *n == pool_of[far_si])
                    && e.far == Some((far.delta().abs(), statements[far_si].currency))
                    && (e.date - near.book_date).num_days().abs() <= matching::MAX_TOLERANCE
            })
            .min_by_key(|(_, e)| (e.date - near.book_date).num_days().abs())
            .map(|(ei, _)| ei)
    };
    let mut reserved: HashMap<&str, HashSet<usize>> = HashMap::new();
    let mut n_converted = 0usize;
    for &(si, li) in &flat {
        let line = &statements[si].lines[li];
        if !line.delta().is_sign_negative() || paired.contains(&(si, li)) {
            continue;
        }
        let candidates: Vec<(usize, usize)> = flat
            .iter()
            .copied()
            .filter(|&(sj, lj)| {
                let other = &statements[sj].lines[lj];
                statements[sj].currency != statements[si].currency
                    && statements[sj].account_no != statements[si].account_no
                    && !paired.contains(&(sj, lj))
                    && other.book_date == line.book_date
                    && other.delta().is_sign_positive()
                    && names(line, &statements[sj])
                    && names(other, &statements[si])
            })
            .collect();
        // Amounts in two currencies can't be compared directly, so the app's
        // record decides which half belongs to which; without one, only an
        // unambiguous single candidate pairs.
        let recorded = candidates.iter().copied().find(|&(sj, lj)| {
            let taken = reserved.get(pool_of[si].as_str());
            conversion_record(si, line, sj, &statements[sj].lines[lj], taken).is_some()
        });
        let counterpart = match (recorded, candidates.as_slice()) {
            (Some(c), _) => Some(c),
            (None, [only]) => Some(*only),
            (None, _) => None,
        };
        let Some((sj, lj)) = counterpart else { continue };
        paired.insert((si, li));
        paired.insert((sj, lj));
        partner.insert((si, li), (sj, lj));
        n_converted += 1;

        let other = &statements[sj].lines[lj];
        for (near_si, near, far_si, far) in [(si, line, sj, other), (sj, other, si, line)] {
            let taken = reserved.entry(pool_of[near_si].as_str()).or_default();
            if let Some(ei) = conversion_record(near_si, near, far_si, far, Some(taken)) {
                taken.insert(ei);
            }
        }
    }

    // Debits the bank reversed, keyed by the debit, and every line in such a pair.
    // Withheld from matching: an app record the same size would otherwise be
    // spent on a movement that never happened.
    let mut reversed_by: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    for (si, s) in statements.iter().enumerate() {
        for (debit, reversal) in s.reversals() {
            reversed_by.insert((si, debit), (si, reversal));
        }
    }
    let in_reversal: HashSet<(usize, usize)> =
        reversed_by.iter().flat_map(|(debit, reversal)| [*debit, *reversal]).collect();

    let mut assignment: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (&pool, events) in &pools {
        let currencies: BTreeSet<Currency> = (0..statements.len())
            .filter(|&si| pool_of[si] == pool)
            .map(|si| statements[si].currency)
            .collect();
        for currency in currencies {
            let to_match: Vec<(usize, usize)> = flat
                .iter()
                .copied()
                .filter(|&(si, li)| {
                    pool_of[si] == pool
                        && statements[si].currency == currency
                        && !is_internal(si, li)
                        && !paired.contains(&(si, li))
                        && !in_reversal.contains(&(si, li))
                })
                .collect();
            let candidates: Vec<usize> = (0..events.len())
                .filter(|ei| {
                    events[*ei].currency == currency
                        && !reserved.get(pool).is_some_and(|r| r.contains(ei))
                })
                .collect();
            let keys: Vec<(NaiveDate, Decimal)> = to_match
                .iter()
                .map(|(si, li)| {
                    let l = &statements[*si].lines[*li];
                    (l.book_date, l.delta())
                })
                .collect();
            let projected: Vec<(NaiveDate, Decimal)> =
                candidates.iter().map(|&ei| (events[ei].date, events[ei].delta)).collect();
            for (i, m) in matching::match_subsets(&keys, &projected).into_iter().enumerate() {
                if let Some(subset) = m {
                    assignment.insert(to_match[i], subset.iter().map(|&k| candidates[k]).collect());
                }
            }
        }
    }
    // Records already used, per pool; the backfill and unmatched pass skip them.
    let mut claimed: HashMap<&str, HashSet<usize>> = reserved;
    for (&(si, _), subset) in &assignment {
        claimed.entry(pool_of[si].as_str()).or_default().extend(subset.iter().copied());
    }
    let institution_claimed: HashSet<usize> =
        claimed.get(institution_app).cloned().unwrap_or_default();

    let mut cathay: Vec<Directive> = Vec::new();
    let mut used_accounts: BTreeSet<String> = BTreeSet::new();
    let mut unmapped: BTreeSet<String> = BTreeSet::new();
    // Which [overrides] entries actually matched a record. An id that matches
    // none is a typo in a 36-character UUID, and without this it would correct
    // nothing and say nothing.
    let mut used_overrides: BTreeSet<String> = BTreeSet::new();
    let (mut n_matched, mut n_internal, mut n_fallback) = (0usize, 0usize, 0usize);
    let mut n_in_transit = 0usize;
    let mut n_reversed = 0usize;
    let mut n_backfill = 0usize;
    // Accounts whose opening the backfill derives; other statements open
    // themselves.
    let mut derived_openings: BTreeMap<&str, (Decimal, NaiveDate)> = BTreeMap::new();

    // --- history from before the statements begin ---
    //
    // The bank has no older statement, so 天天記帳 is the only source for
    // 2022-2025. It records a single 國泰 account, so activity is attributed by
    // rule: ETF flows move through 投資, everything else through 活存. 投資 is a
    // pass-through — 活存 funds each settlement the same day — because the real
    // funding transfers were internal to Cathay and were never recorded anywhere.
    let anchor = statements
        .iter()
        .filter_map(|s| s.lines.first().map(|l| l.book_date))
        .min()
        .context("statements contain no lines")?;
    let backfilling = opts.backfill && !app_events.is_empty();

    if backfilling {
        // A claimed record must not be replayed here. The date tolerance reaches
        // back over the boundary, so records dated just before the first
        // statement line are often claimed by it — emitting them again would
        // double the expense and inflate the derived opening to hide it.
        let pre: Vec<&daily::AppEvent> = app_events
            .iter()
            .enumerate()
            .filter(|(i, e)| e.date < anchor && !institution_claimed.contains(i))
            .map(|(_, e)| e)
            .collect();
        if let Some(start) = pre.first().map(|e| e.date) {
            let opening_of = |want: &str| -> Decimal {
                statements
                    .iter()
                    .find(|s| {
                        statement_account(&chart, &s.account_no)
                            .map(|a| a.as_ref() == want)
                            .unwrap_or(false)
                    })
                    .map(|s| s.opening_balance())
                    .unwrap_or_default()
            };
            // 活存 absorbs every pre-anchor movement, so working back from its
            // statement opening gives what the book must have started at. This is
            // DERIVED, not observed — no bank record of it exists.
            let drift: Decimal = pre.iter().map(|e| e.delta).sum();
            let savings_open = opening_of(savings) - drift;

            cathay
                .push(Directive::Comment(format!(";; --- 天天記帳 history before {} ---", anchor)));
            cathay.push(Directive::Comment(
                ";; Opening balances below are DERIVED by working backwards from".to_string(),
            ));
            cathay.push(Directive::Comment(
                ";; the first statement. No bank record of them exists.".to_string(),
            ));
            cathay.push(Directive::Transaction(model::Transaction {
                date: start,
                payee: "Opening balance".to_string(),
                narration: "derived from 天天記帳, not observed".to_string(),
                tags: Vec::new(),
                postings: vec![
                    writer::Posting::new(savings, savings_open, Currency::TWD),
                    writer::Posting::new(investment, opening_of(investment), Currency::TWD),
                    writer::Posting::inferred(model::OPENING_EQUITY),
                ],
                source: model::Source::Import,
                external_ref: None,
            }));
            cathay.push(Directive::Blank);
            used_accounts.insert(savings.to_string());
            used_accounts.insert(investment.to_string());
            derived_openings.insert(savings, (savings_open, start));
            derived_openings.insert(investment, (opening_of(investment), start));

            for event in &pre {
                let (target, tags) = resolve(&chart, event, "", &mut unmapped, &mut used_overrides);
                used_accounts.insert(target.clone());
                let raw = match &event.contra {
                    daily::Contra::Category(n) | daily::Contra::Account(n) => n.as_str(),
                };
                let memo = if event.memo.is_empty() { raw } else { &event.memo };
                let narration = narration_for(&chart, &event.id, memo);

                let external_ref = (!event.id.is_empty()).then(|| event.id.clone());
                if matches!(&event.contra, daily::Contra::Account(n) if *n == settlement_source) {
                    // The 資金調撥 funding leg is synthetic (no source row of its
                    // own), so only the settlement transaction carries the
                    // record's id — else the two would collide on (source, ref).
                    cathay.push(Directive::Transaction(model::Transaction {
                        date: event.date,
                        payee: "資金調撥".to_string(),
                        narration: "活存 funds 投資 for settlement".to_string(),
                        tags: Vec::new(),
                        postings: vec![
                            writer::Posting::new(savings, event.delta, event.currency),
                            writer::Posting::new(investment, -event.delta, event.currency),
                        ],
                        source: model::Source::Tiantian,
                        external_ref: None,
                    }));
                    cathay.push(Directive::Blank);
                    cathay.push(Directive::Transaction(model::Transaction {
                        date: event.date,
                        payee: settlement_source.clone(),
                        narration: narration.to_string(),
                        tags,
                        postings: vec![
                            writer::Posting::new(investment, event.delta, event.currency),
                            contra_posting(target, event),
                        ],
                        source: model::Source::Tiantian,
                        external_ref,
                    }));
                } else {
                    cathay.push(Directive::Transaction(model::Transaction {
                        date: event.date,
                        payee: String::new(),
                        narration: narration.to_string(),
                        tags,
                        postings: vec![
                            writer::Posting::new(savings, event.delta, event.currency),
                            contra_posting(target, event),
                        ],
                        source: model::Source::Tiantian,
                        external_ref,
                    }));
                }
                cathay.push(Directive::Blank);
                n_backfill += 1;
            }
        }
    }

    for (si, statement) in statements.iter().enumerate() {
        let account: &str = statement_account(&chart, &statement.account_no)?;
        let currency = statement.currency;
        used_accounts.insert(account.to_string());
        let first = statement.lines.first().expect("load_bank_statement rejects empty statements");

        let paths: Vec<String> =
            statement_paths[si].iter().map(|p| p.display().to_string()).collect();
        cathay.push(Directive::Comment(format!(
            ";; --- {} {} — {} rows from {} ---",
            statement.account_no,
            statement.account_kind,
            statement.lines.len(),
            paths.join(", ")
        )));
        if !derived_openings.contains_key(account) {
            // Dated the day before, so the assertion below still checks something:
            // Beancount asserts at the start of the day.
            cathay.push(Directive::Transaction(model::Transaction {
                date: first.book_date.pred_opt().unwrap_or(first.book_date),
                payee: "Opening balance".to_string(),
                narration: statement.account_kind.clone(),
                tags: Vec::new(),
                postings: vec![
                    writer::Posting::new(account, statement.opening_balance(), currency),
                    writer::Posting::inferred(model::OPENING_EQUITY),
                ],
                source: model::Source::Import,
                external_ref: None,
            }));
            cathay.push(Directive::Blank);
        }
        cathay.push(Directive::Balance(model::Balance {
            date: first.book_date,
            account: account.to_string(),
            amount: statement.opening_balance(),
            currency,
        }));
        cathay.push(Directive::Blank);

        let refs = statement.dedup_refs();
        for (li, line) in statement.lines.iter().enumerate() {
            // The receiving half of a paired transfer is emitted by its partner.
            if paired.contains(&(si, li)) && !partner.contains_key(&(si, li)) {
                continue;
            }
            // Likewise a bank reversal, which its debit emits.
            if in_reversal.contains(&(si, li)) && !reversed_by.contains_key(&(si, li)) {
                continue;
            }

            let mut postings = vec![writer::Posting::new(account, line.delta(), currency)];
            let mut tags: Vec<String> = Vec::new();
            let reversal =
                reversed_by.get(&(si, li)).map(|&(rsi, rli)| &statements[rsi].lines[rli]);
            // A reversed debit is labelled by the reversal, which says why it nets to
            // nothing.
            let description = match reversal {
                Some(reversal) => reversal.description.as_str(),
                None => line.description.as_str(),
            };

            if let Some(reversal) = reversal {
                // Both rows stay on the account, so the journal still shows the
                // attempt, and they cancel within the one transaction.
                n_reversed += 1;
                postings.push(writer::Posting::new(account, reversal.delta(), currency));
            } else if let Some(&(psi, pli)) = partner.get(&(si, li)) {
                n_internal += 1;
                let far_account: &str = statement_account(&chart, &statements[psi].account_no)?;
                used_accounts.insert(far_account.to_string());
                let far_currency = statements[psi].currency;
                let far = writer::Posting::new(
                    far_account,
                    statements[psi].lines[pli].delta(),
                    far_currency,
                );
                postings.push(if far_currency == currency {
                    far
                } else {
                    far.worth(line.delta().abs(), currency)
                });
            } else if is_internal(si, li) {
                n_in_transit += 1;
                postings.push(writer::Posting::new(clearing, -line.delta(), currency));
            } else if let Some(subset) = assignment.get(&(si, li)) {
                n_matched += 1;
                let events = pools.get(pool_of[si].as_str()).unwrap_or(&no_events);
                for ei in subset {
                    let event = &events[*ei];
                    let (target, event_tags) = resolve(
                        &chart,
                        event,
                        &line.description,
                        &mut unmapped,
                        &mut used_overrides,
                    );
                    tags.extend(event_tags);
                    used_accounts.insert(target.clone());
                    postings.push(contra_posting(target, event));
                }
            } else {
                n_fallback += 1;
                let target: &str = fallback_account(&chart, &line.description, line.delta());
                used_accounts.insert(target.to_string());
                postings.push(writer::Posting::new(target, -line.delta(), currency));
            }

            let narration = [line.info.as_str(), line.memo.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            tags.sort();
            tags.dedup();
            cathay.push(Directive::Transaction(model::Transaction {
                date: line.book_date,
                // A reversed debit is labelled by its reversal (see `description`
                // above), so the journal says why the pair nets to nothing.
                payee: description.to_string(),
                narration,
                tags,
                postings,
                source: model::Source::Import,
                external_ref: Some(refs[li].clone()),
            }));
            cathay.push(Directive::Blank);
        }

        cathay.push(Directive::Balance(model::Balance {
            date: statement.assert_date(),
            account: account.to_string(),
            amount: statement.closing_balance(),
            currency,
        }));
        cathay.push(Directive::Blank);
    }

    // 天天記帳 records touching 國泰 that no statement line matched.
    //
    // Dropping these loses the far side of a real movement — a broker account was
    // short by a five-figure sum because four transfers never matched. The
    // statement line for the same movement has already been emitted with an
    // uncategorised contra, so the near side goes to the same bucket, where the
    // two cancel: the far account gets its posting and Cathay's asserted
    // balance is untouched.
    let mut n_unmatched_records = 0;
    for (&pool, events) in &pools {
        let pool_claimed = claimed.get(pool);
        for (index, event) in events.iter().enumerate() {
            // Only the institution pool has a backfill.
            let backfill_era = pool == institution_app && event.date < anchor;
            if pool_claimed.is_some_and(|c| c.contains(&index))
                || backfill_era
                || event.delta.is_zero()
            {
                continue;
            }
            let (target, tags) = resolve(&chart, event, "", &mut unmapped, &mut used_overrides);
            used_accounts.insert(target.clone());
            let near: &str = if event.delta.is_sign_negative() {
                &chart.fallback.expense
            } else {
                &chart.fallback.income
            };
            used_accounts.insert(near.to_string());
            cathay.push(Directive::Transaction(model::Transaction {
                date: event.date,
                payee: "未對應紀錄".to_string(),
                narration: narration_for(&chart, &event.id, &event.memo).to_string(),
                tags,
                postings: vec![
                    contra_posting(target, event),
                    writer::Posting::new(near, event.delta, event.currency),
                ],
                source: model::Source::Tiantian,
                external_ref: (!event.id.is_empty()).then(|| event.id.clone()),
            }));
            cathay.push(Directive::Blank);
            n_unmatched_records += 1;
        }
    }

    used_accounts.insert(clearing.to_string());

    // Accounts with no bank statement, taken from 天天記帳 as recorded.
    let statement_pools: BTreeSet<&str> = pools.keys().copied().collect();
    let (daily, n_other) = emit_daily_accounts(
        &entries,
        &chart,
        &statement_pools,
        &mut used_accounts,
        &mut unmapped,
        &mut used_overrides,
    );

    // Assert every non-國泰 asset/liability balance against the app's own figure,
    // so bean-check proves the emitted ledger matches 天天記帳. Beancount checks a
    // balance at the start of its date, so assert the day after the last entry.
    let last_date = entries
        .iter()
        .map(daily::Entry::date)
        .chain(statements.iter().flat_map(|s| s.lines.iter().map(|l| l.book_date)))
        .max()
        .unwrap_or(anchor);
    let assert_date = last_date.succ_opt().unwrap_or(last_date);
    let bank_accounts: Vec<&str> = statements
        .iter()
        .map(|s| statement_account(&chart, &s.account_no).map(|a| a.as_ref()))
        .collect::<Result<_>>()?;
    let bank_asserted: BTreeSet<&str> = bank_accounts.iter().copied().collect();
    let leaf_bal = app_balances(&entries, &chart, &statement_pools);
    let accounts: BTreeSet<&str> = leaf_bal.keys().map(|(a, _)| a.as_str()).collect();
    let mut asserts: Vec<Directive> = Vec::new();
    let mut n_asserted = 0usize;

    // Assert every used asset or liability account, except 國泰's — those are
    // asserted against the bank statement, not the app.
    let assertable = accounts.into_iter().filter(|&account| {
        let root = account.split(':').next().unwrap_or("").parse::<AccountType>();
        used_accounts.contains(account)
            && matches!(root, Ok(AccountType::Assets | AccountType::Liabilities))
            && !bank_asserted.contains(account)
    });
    for account in assertable {
        // A Beancount balance assertion covers the account's whole subtree, so
        // the asserted figure must sum the account with its descendants.
        let prefix = format!("{account}:");
        let mut by_currency: BTreeMap<Currency, Decimal> = BTreeMap::new();
        for ((a, currency), amount) in &leaf_bal {
            if a == account || a.starts_with(&prefix) {
                *by_currency.entry(*currency).or_default() += *amount;
            }
        }
        for (a, currency, amount, _) in &declared {
            if a == account || a.starts_with(&prefix) {
                *by_currency.entry(*currency).or_default() += *amount;
            }
        }
        // Emit an account's assertions ordered by currency code, not by the
        // enum's declaration order, so the output is stable and alphabetical.
        let mut per_currency: Vec<(Currency, Decimal)> = by_currency.into_iter().collect();
        per_currency.sort_by_key(|(currency, _)| currency.to_string());
        for (currency, amount) in per_currency {
            asserts.push(Directive::Balance(model::Balance {
                date: assert_date,
                account: account.to_string(),
                amount,
                currency,
            }));
            n_asserted += 1;
        }
    }

    // Opening balances predate every record, so build their transactions first
    // and make sure their accounts are opened alongside the rest.
    let mut openings: Vec<Directive> = Vec::new();
    let mut superseded_openings: BTreeSet<String> = BTreeSet::new();
    for (account, currency, amount, date) in &declared {
        // The backfill or a statement already opens this account; the two must
        // agree, else neither may silently win.
        let derived = derived_openings
            .get(account.as_str())
            .filter(|_| *currency == Currency::TWD)
            .map(|&(expected, from)| (expected, from, "the backfill"));
        let covering = derived.or_else(|| {
            statements
                .iter()
                .zip(&bank_accounts)
                .find(|(s, a)| **a == account && s.currency == *currency)
                .map(|(s, _)| {
                    let first = s.lines.first().expect("load rejects empty statements");
                    (s.opening_balance(), first.book_date, "its statement")
                })
        });
        if let Some((expected, from, by)) = covering {
            anyhow::ensure!(
                *amount == expected && *date <= from,
                "opening {account} {currency} = {amount} on {date} contradicts {by}, which opens \
                 at {expected} before {from}"
            );
            superseded_openings.insert(format!("{account} {currency}"));
            continue;
        }
        used_accounts.insert(account.clone());
        openings.push(Directive::Transaction(model::Transaction {
            date: *date,
            payee: String::new(),
            narration: "Opening balance".to_string(),
            tags: Vec::new(),
            postings: vec![
                writer::Posting::new(account.clone(), *amount, *currency),
                writer::Posting::inferred(model::OPENING_EQUITY),
            ],
            // Declared by hand; the ref makes a second opening for the pair
            // fail the dedup index.
            source: model::Source::Manual,
            external_ref: Some(model::opening_ref(account, *currency)),
        }));
    }

    let summary = Summary {
        records_start,
        folded: n_folded,
        converted: n_converted,
        superseded_openings,
        other_accounts: n_other,
        unmatched_records: n_unmatched_records,
        output: None,
        anchor,
        backfilled: n_backfill,
        categorised: n_matched,
        internal: n_internal,
        in_transit: n_in_transit,
        reversed: n_reversed,
        uncategorised: n_fallback,
        balance_assertions: n_asserted,
        stale_overrides: chart.stale_overrides(&used_overrides),
        unmapped,
    };
    let ledger_model = model::Model { cathay, daily, asserts, openings, opens: used_accounts };
    Ok((ledger_model, summary))
}

/// Assembles the ledger and writes the generated Beancount files, returning the
/// run summary. This is the `ledger` command; the freeze tool calls
/// [`assemble`] instead and never touches the text.
pub fn build(opts: &Args) -> Result<Summary> {
    let (model, mut summary) = assemble(opts)?;
    let out_dir = opts.ledger_dir.join("generated");
    std::fs::create_dir_all(&out_dir)?;
    write_files(&out_dir, &model)?;
    summary.output = Some(out_dir.join("cathay.beancount"));
    Ok(summary)
}

/// Serialises the model to the three generated files, each with its header.
fn write_files(out_dir: &std::path::Path, ledger: &model::Model) -> Result<()> {
    let mut accounts = String::new();
    accounts.push_str(";; GENERATED — do not edit by hand.\n\n");
    for account in &ledger.opens {
        writeln!(accounts, "2000-01-01 open {}", account)?;
    }
    if !ledger.openings.is_empty() {
        accounts.push_str(
            "\n;; Opening balances — positions predating 天天記帳, emitted against\n;; \
             Equity:Opening-Balances and folded into the assertions in daily.beancount\n;; so \
             bean-check still proves the ledger matches the app plus this start.\n\n",
        );
        accounts.push_str(&model::render(&ledger.openings));
    }
    std::fs::write(out_dir.join("accounts.beancount"), accounts)?;

    let mut cathay = String::new();
    cathay.push_str(";; GENERATED — do not edit by hand.\n");
    cathay.push_str(";; Balance assertions come from the statement's own 餘額 column, which is\n");
    cathay.push_str(";; independent of the 提出/存入 columns the transactions are built from.\n\n");
    cathay.push_str(&model::render(&ledger.cathay));
    std::fs::write(out_dir.join("cathay.beancount"), cathay)?;

    if !ledger.daily.is_empty() || !ledger.asserts.is_empty() {
        let mut daily = String::new();
        daily.push_str(";; GENERATED — do not edit by hand.\n");
        daily.push_str(";; Accounts with no bank statement, taken from 天天記帳 as recorded.\n");
        daily.push_str(
            ";; Records touching 國泰 are not here — those come from its statements.\n\n",
        );
        daily.push_str(&model::render(&ledger.daily));
        if !ledger.asserts.is_empty() {
            daily.push_str(
                "\n;; Balance assertions computed straight from 天天記帳, independent of\n",
            );
            daily.push_str(
                ";; how the postings above are emitted. bean-check thus proves the ledger\n",
            );
            daily.push_str(";; matches the app for every non-國泰 asset and liability account.\n");
            daily.push_str(&model::render(&ledger.asserts));
        }
        std::fs::write(out_dir.join("daily.beancount"), daily)?;
    }
    Ok(())
}
