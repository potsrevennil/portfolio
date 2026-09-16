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
    daily,
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
/// restating what the importer just wrote. 國泰 itself is excluded: it is
/// driven by the bank statement, and asserted against it.
fn app_balances(
    entries: &[daily::Entry],
    chart: &accounts::Chart,
) -> BTreeMap<(String, Currency), Decimal> {
    let mut bal: BTreeMap<(String, Currency), Decimal> = BTreeMap::new();
    let app_account = chart.institution.app_account.as_str();
    for entry in entries {
        let mut add = |name: &str, amount: Decimal, currency: Currency| {
            if name == app_account {
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

    // Both exports are read once, here. 天天記帳 lumps the two Cathay accounts
    // into a single 國泰 account, so its records are matched against the two
    // statements pooled together; `view` is that account's slice of this parse.
    let entries = match (opts.daily_income_expense.as_deref(), opts.daily_transfers.as_deref()) {
        (Some(ie), Some(xf)) => daily::load_entries(ie, xf)?,
        _ => Vec::new(),
    };
    let app_events = daily::view(&entries, &chart.institution.app_account);

    // Load each statement with the path it came from, then order by the
    // statement's earliest line rather than by argument order. A per-account
    // `.find()` (the derived-opening backfill below) must reach the
    // chronologically first statement for an account; argument order made a
    // second 活存 export passed first silently drive the opening balance.
    let mut loaded: Vec<(statements::cathay::BankStatement, std::path::PathBuf)> = opts
        .cathay_statements
        .iter()
        .map(|path| statements::cathay::load(path).map(|s| (s, path.clone())))
        .collect::<Result<_>>()?;
    loaded.sort_by(|a, b| {
        let key = |s: &statements::cathay::BankStatement| {
            (s.lines.first().map(|l| l.book_date), s.account_no.clone())
        };
        key(&a.0).cmp(&key(&b.0))
    });
    let (statements, statement_paths): (Vec<statements::cathay::BankStatement>, Vec<_>) =
        loaded.into_iter().unzip();

    // Flatten to (statement index, line index) so lines from both accounts can be
    // matched against one pool of app records.
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

    let to_match: Vec<(usize, usize)> = flat
        .iter()
        .copied()
        .filter(|(si, li)| !is_internal(*si, *li) && !in_reversal.contains(&(*si, *li)))
        .collect();
    let keys: Vec<(NaiveDate, Decimal)> = to_match
        .iter()
        .map(|(si, li)| {
            let l = &statements[*si].lines[*li];
            (l.book_date, l.delta())
        })
        .collect();
    let matched = matching::match_lines(&keys, &app_events);

    let mut assignment: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (i, m) in matched.iter().enumerate() {
        if let Some(subset) = m {
            assignment.insert(to_match[i], subset.clone());
        }
    }
    // Records the matcher has already spent on a statement line. Both the
    // backfill and the unmatched-record pass below need to exclude these.
    let claimed: HashSet<usize> = assignment.values().flat_map(|v| v.iter().copied()).collect();

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
            .filter(|(i, e)| e.date < anchor && !claimed.contains(i))
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
                    writer::Posting::inferred("Equity:Opening-Balances"),
                ],
                source: model::Source::Import,
                external_ref: None,
            }));
            cathay.push(Directive::Blank);
            used_accounts.insert(savings.to_string());
            used_accounts.insert(investment.to_string());

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

        cathay.push(Directive::Comment(format!(
            ";; --- {} {} — {} rows from {} ---",
            statement.account_no,
            statement.account_kind,
            statement.lines.len(),
            statement_paths[si].display()
        )));
        if !backfilling {
            // Dated the day before, so the assertion below still checks something:
            // Beancount asserts at the start of the day.
            cathay.push(Directive::Transaction(model::Transaction {
                date: first.book_date.pred_opt().unwrap_or(first.book_date),
                payee: "Opening balance".to_string(),
                narration: statement.account_kind.clone(),
                tags: Vec::new(),
                postings: vec![
                    writer::Posting::new(account, statement.opening_balance(), currency),
                    writer::Posting::inferred("Equity:Opening-Balances"),
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
                postings.push(writer::Posting::new(
                    far_account,
                    statements[psi].lines[pli].delta(),
                    currency,
                ));
            } else if is_internal(si, li) {
                n_in_transit += 1;
                postings.push(writer::Posting::new(clearing, -line.delta(), currency));
            } else if let Some(subset) = assignment.get(&(si, li)) {
                n_matched += 1;
                for ei in subset {
                    let event = &app_events[*ei];
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
                external_ref: Some(line.dedup_ref(&statement.account_no)),
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
    for (index, event) in app_events.iter().enumerate() {
        if claimed.contains(&index) || event.date < anchor || event.delta.is_zero() {
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

    used_accounts.insert(clearing.to_string());

    // Accounts with no bank statement, taken from 天天記帳 as recorded.
    let (daily, n_other) = emit_daily_accounts(
        &entries,
        &chart,
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
    let bank_asserted: BTreeSet<&str> =
        chart.institution.accounts.values().map(|a| a.as_ref()).collect();
    let leaf_bal = app_balances(&entries, &chart);
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
        // Fold in any opening balance on this account (or its subtree): the app
        // records net only the movements since, so the asserted figure must add
        // the declared starting position the build also emits below.
        for (ob_account, ob) in &chart.opening_balances {
            if ob_account == account || ob_account.starts_with(&prefix) {
                *by_currency.entry(ob.currency).or_default() += ob.amount;
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
    let mut declared: Vec<(&String, &accounts::OpeningBalance)> =
        chart.opening_balances.iter().collect();
    declared.sort_by(|a, b| a.0.cmp(b.0));
    for (account, ob) in declared {
        used_accounts.insert(account.clone());
        openings.push(Directive::Transaction(model::Transaction {
            date: ob.date,
            payee: String::new(),
            narration: "Opening balance".to_string(),
            tags: Vec::new(),
            postings: vec![
                writer::Posting::new(account.clone(), ob.amount, ob.currency),
                writer::Posting::inferred("Equity:Opening-Balances"),
            ],
            source: model::Source::Import,
            external_ref: None,
        }));
    }

    let summary = Summary {
        other_accounts: n_other,
        unmatched_records: n_unmatched_records,
        output: ledger.join("generated").join("cathay.beancount"),
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
    let (model, summary) = assemble(opts)?;
    let out_dir = opts.ledger_dir.join("generated");
    std::fs::create_dir_all(&out_dir)?;
    write_files(&out_dir, &model)?;
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
