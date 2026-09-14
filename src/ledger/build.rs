//! Assembles the ledger: statements first, then everything 天天記帳 covers.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::Write as _,
    path::Path,
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::{
    accounts::{self, Account, AccountType},
    args::Args,
    daily,
    emit::{contra_posting, emit_daily_accounts, narration_for, resolve},
    matching,
    names::{fallback_account, statement_account},
    statements,
    summary::Summary,
    writer,
};

/// Each account's balance as the app's own records imply it, per currency.
///
/// Computed straight from 天天記帳, with no knowledge of how the ledger emits its
/// postings — a 國泰 transfer's far side, a matched statement line, the backfill,
/// all reach the same account by different code paths above, and this sums none
/// of that, only the raw records. Asserting these values therefore makes
/// `bean-check` prove the emitted ledger matches the app, rather than restating
/// what the importer just wrote. 國泰 itself is excluded: it is driven by the
/// bank statement, and asserted against it.
fn app_balances(
    entries: &[daily::Entry],
    chart: &accounts::Chart,
) -> BTreeMap<(String, String), Decimal> {
    let mut bal: BTreeMap<(String, String), Decimal> = BTreeMap::new();
    let app_account = chart.institution.app_account.as_str();
    for entry in entries {
        let mut add = |name: &str, amount: Decimal, currency: &str| {
            if name == app_account {
                return;
            }
            if let Some(mapping) = chart.account(name) {
                *bal.entry((mapping.account.to_string(), currency.to_string())).or_default() +=
                    amount;
            }
        };
        match entry {
            daily::Entry::Flow { account, amount, currency, .. } => {
                add(account, *amount, currency);
            }
            daily::Entry::Transfer { from, out, out_currency, to, inn, in_currency, .. } => {
                add(from, -*out, out_currency);
                add(to, *inn, in_currency);
            }
        }
    }
    bal
}

pub fn build(opts: &Args) -> Result<Summary> {
    let ledger = Path::new(&opts.ledger_dir);
    let out_dir = ledger.join("generated");
    std::fs::create_dir_all(&out_dir)?;

    let chart = accounts::Chart::load(
        ledger.join("mapping.toml").to_str().context("non-utf8 ledger path")?,
    )?;
    // Accounts the build has to reach by role rather than by name, since the
    // name is the config's to choose.
    let savings = chart.institution.primary.as_str();
    let investment = chart.institution.settlement.as_str();
    let clearing = chart.institution.clearing.as_str();
    let settlement_source = chart.institution.settlement_app_account.clone();

    // Both exports are read once, here. 天天記帳 lumps the two Cathay accounts
    // into a single 國泰 account, so its records are matched against the two
    // statements pooled together; `view` is that account's slice of this parse.
    let entries = match (opts.daily_income_expense.as_deref(), opts.daily_transfers.as_deref()) {
        (Some(ie), Some(xf)) => daily::load_entries(ie, xf)?,
        _ => Vec::new(),
    };
    let app_events = daily::view(&entries, &chart.institution.app_account);

    let statements: Vec<statements::cathay::BankStatement> = opts
        .cathay_statements
        .iter()
        .map(|p| statements::cathay::load(p))
        .collect::<Result<_>>()?;

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

    let to_match: Vec<(usize, usize)> =
        flat.iter().copied().filter(|(si, li)| !is_internal(*si, *li)).collect();
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

    let mut body = String::new();
    let mut used_accounts: BTreeSet<String> = BTreeSet::new();
    let mut unmapped: BTreeSet<String> = BTreeSet::new();
    // Which [overrides] entries actually matched a record. An id that matches
    // none is a typo in a 36-character UUID, and without this it would correct
    // nothing and say nothing.
    let mut used_overrides: BTreeSet<String> = BTreeSet::new();
    let (mut n_matched, mut n_internal, mut n_fallback) = (0usize, 0usize, 0usize);
    let mut n_in_transit = 0usize;
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
                    .find(|s| statement_account(&chart, &s.account_no).map(|a| a.as_str() == want).unwrap_or(false))
                    .map(|s| s.opening_balance())
                    .unwrap_or_default()
            };
            // 活存 absorbs every pre-anchor movement, so working back from its
            // statement opening gives what the book must have started at. This is
            // DERIVED, not observed — no bank record of it exists.
            let drift: Decimal = pre.iter().map(|e| e.delta).sum();
            let savings_open = opening_of(savings) - drift;

            writeln!(body, ";; --- 天天記帳 history before {} ---", anchor)?;
            writeln!(body, ";; Opening balances below are DERIVED by working backwards from")?;
            writeln!(body, ";; the first statement. No bank record of them exists.")?;
            body.push_str(&writer::transaction(
                start,
                "Opening balance",
                "derived from 天天記帳, not observed",
                &[],
                &[
                    writer::Posting::new(savings, savings_open, "TWD"),
                    writer::Posting::new(investment, opening_of(investment), "TWD"),
                    writer::Posting::inferred("Equity:Opening-Balances"),
                ],
            ));
            body.push('\n');
            used_accounts.insert(savings.to_string());
            used_accounts.insert(investment.to_string());

            for event in &pre {
                let (target, tags) =
                    resolve(&chart, event, "", &mut unmapped, &mut used_overrides);
                used_accounts.insert(target.clone());
                let raw = match &event.contra {
                    daily::Contra::Category(n) | daily::Contra::Account(n) => n.as_str(),
                };
                let memo = if event.memo.is_empty() { raw } else { &event.memo };
                let narration = narration_for(&chart, &event.id, memo);

                if matches!(&event.contra, daily::Contra::Account(n) if *n == settlement_source) {
                    body.push_str(&writer::transaction(
                        event.date,
                        "資金調撥",
                        "活存 funds 投資 for settlement",
                        &[],
                        &[
                            writer::Posting::new(savings, event.delta, &event.currency),
                            writer::Posting::new(investment, -event.delta, &event.currency),
                        ],
                    ));
                    body.push('\n');
                    body.push_str(&writer::transaction(
                        event.date,
                        &settlement_source,
                        narration,
                        &tags,
                        &[
                            writer::Posting::new(investment, event.delta, &event.currency),
                            contra_posting(target, event),
                        ],
                    ));
                } else {
                    body.push_str(&writer::transaction(
                        event.date,
                        "",
                        narration,
                        &tags,
                        &[
                            writer::Posting::new(savings, event.delta, &event.currency),
                            contra_posting(target, event),
                        ],
                    ));
                }
                body.push('\n');
                n_backfill += 1;
            }
        }
    }

    for (si, statement) in statements.iter().enumerate() {
        let account = statement_account(&chart, &statement.account_no)?.as_str();
        let currency = &statement.currency;
        used_accounts.insert(account.to_string());
        let first = statement.lines.first().expect("load_bank_statement rejects empty statements");

        writeln!(
            body,
            ";; --- {} {} — {} rows from {} ---",
            statement.account_no,
            statement.account_kind,
            statement.lines.len(),
            opts.cathay_statements[si]
        )?;
        if !backfilling {
            // Dated the day before, so the assertion below still checks something:
            // Beancount asserts at the start of the day.
            body.push_str(&writer::transaction(
                first.book_date.pred_opt().unwrap_or(first.book_date),
                "Opening balance",
                &statement.account_kind,
                &[],
                &[
                    writer::Posting::new(account, statement.opening_balance(), currency),
                    writer::Posting::inferred("Equity:Opening-Balances"),
                ],
            ));
            body.push('\n');
        }
        body.push_str(&writer::balance(
            first.book_date,
            account,
            statement.opening_balance(),
            currency,
        ));
        body.push('\n');

        for (li, line) in statement.lines.iter().enumerate() {
            // The receiving half of a paired transfer is emitted by its partner.
            if paired.contains(&(si, li)) && !partner.contains_key(&(si, li)) {
                continue;
            }

            let mut postings =
                vec![writer::Posting::new(account, line.delta(), currency)];
            let mut tags: Vec<String> = Vec::new();

            if let Some(&(psi, pli)) = partner.get(&(si, li)) {
                n_internal += 1;
                let far_account = statement_account(&chart, &statements[psi].account_no)?.as_str();
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
                let target = fallback_account(&chart, &line.description, line.delta()).as_str();
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
            body.push_str(&writer::transaction(
                line.book_date,
                &line.description,
                &narration,
                &tags,
                &postings,
            ));
            body.push('\n');
        }

        body.push_str(&writer::balance(
            statement.assert_date(),
            account,
            statement.closing_balance(),
            currency,
        ));
        body.push('\n');
    }

    // 天天記帳 records touching 國泰 that no statement line matched.
    //
    // Dropping these loses the far side of a real movement — a broker account was short by a
    // five-figure sum because four transfers never matched. The statement line for the
    // same movement has already been emitted with an uncategorised contra, so
    // the near side goes to the same bucket, where the two cancel: the far
    // account gets its posting and Cathay's asserted balance is untouched.
    let mut n_unmatched_records = 0;
    for (index, event) in app_events.iter().enumerate() {
        if claimed.contains(&index) || event.date < anchor || event.delta.is_zero() {
            continue;
        }
        let (target, tags) = resolve(&chart, event, "", &mut unmapped, &mut used_overrides);
        used_accounts.insert(target.clone());
        let near = if event.delta.is_sign_negative() {
            "Expenses:Uncategorized"
        } else {
            "Income:Uncategorized"
        };
        used_accounts.insert(near.to_string());
        body.push_str(&writer::transaction(
            event.date,
            "未對應紀錄",
            narration_for(&chart, &event.id, &event.memo),
            &tags,
            &[contra_posting(target, event), writer::Posting::new(near, event.delta, &event.currency)],
        ));
        body.push('\n');
        n_unmatched_records += 1;
    }

    used_accounts.insert(clearing.to_string());

    // Accounts with no bank statement, taken from 天天記帳 as recorded.
    let (daily_body, n_other) =
        emit_daily_accounts(&entries, &chart, &mut used_accounts, &mut unmapped, &mut used_overrides);

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
        chart.institution.accounts.values().map(Account::as_str).collect();
    let leaf_bal = app_balances(&entries, &chart);
    let accounts: BTreeSet<&str> = leaf_bal.keys().map(|(a, _)| a.as_str()).collect();
    let mut asserts = String::new();
    let mut n_asserted = 0usize;
    for account in accounts {
        let root = account.split(':').next().unwrap_or("").parse::<AccountType>();
        if !used_accounts.contains(account)
            || !matches!(root, Ok(AccountType::Assets | AccountType::Liabilities))
            || bank_asserted.contains(account)
        {
            continue;
        }
        // A Beancount balance assertion covers the account's whole subtree, so
        // the asserted figure must sum the account with its descendants.
        let prefix = format!("{account}:");
        let mut by_currency: BTreeMap<&str, Decimal> = BTreeMap::new();
        for ((a, currency), amount) in &leaf_bal {
            if a == account || a.starts_with(&prefix) {
                *by_currency.entry(currency.as_str()).or_default() += *amount;
            }
        }
        for (currency, amount) in by_currency {
            asserts.push_str(&writer::balance(assert_date, account, amount, currency));
            n_asserted += 1;
        }
    }

    let mut opens = String::new();
    opens.push_str(";; GENERATED — do not edit by hand.\n\n");
    for account in &used_accounts {
        writeln!(opens, "2000-01-01 open {}", account)?;
    }
    std::fs::write(out_dir.join("accounts.beancount"), opens)?;

    let mut out = String::new();
    out.push_str(";; GENERATED — do not edit by hand.\n");
    out.push_str(";; Balance assertions come from the statement's own 餘額 column, which is\n");
    out.push_str(";; independent of the 提出/存入 columns the transactions are built from.\n\n");
    out.push_str(&body);
    let path = out_dir.join("cathay.beancount");
    std::fs::write(&path, out)?;

    if !daily_body.is_empty() || !asserts.is_empty() {
        let mut out = String::new();
        out.push_str(";; GENERATED — do not edit by hand.\n");
        out.push_str(";; Accounts with no bank statement, taken from 天天記帳 as recorded.\n");
        out.push_str(";; Records touching 國泰 are not here — those come from its statements.\n\n");
        out.push_str(&daily_body);
        if !asserts.is_empty() {
            out.push_str("\n;; Balance assertions computed straight from 天天記帳, independent of\n");
            out.push_str(";; how the postings above are emitted. bean-check thus proves the ledger\n");
            out.push_str(";; matches the app for every non-國泰 asset and liability account.\n");
            out.push_str(&asserts);
        }
        std::fs::write(out_dir.join("daily.beancount"), out)?;
    }

    Ok(Summary {
        other_accounts: n_other,
        unmatched_records: n_unmatched_records,
        output: path,
        anchor,
        backfilled: n_backfill,
        categorised: n_matched,
        internal: n_internal,
        in_transit: n_in_transit,
        uncategorised: n_fallback,
        balance_assertions: n_asserted,
        stale_overrides: chart.stale_overrides(&used_overrides),
        unmapped,
    })
}
