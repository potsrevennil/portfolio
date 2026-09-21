//! Freeze implementation: reconcile the assembled model plus `manual.csv` into
//! a journal, write it, and verify it before trusting it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use model::CONVERSIONS;
use rust_decimal::Decimal;

use super::manual;
use crate::{
    currency::Currency,
    ledger::{
        self,
        accounts::{AccountType, Chart},
        args::Args as BuildArgs,
        journal,
        journal::PLACEHOLDER_TAG,
        model,
        writer::Posting,
    },
    store::{
        assertions::{AssertionSource, BalanceAssertion},
        query::in_subtree,
    },
};

/// The subtree the securities backfill lands in — the ~722k TWD ETF placeholder
/// among it. Every posting here is tagged (with [`journal::PLACEHOLDER_TAG`])
/// so it can be replaced with real positions later without double-counting.
const SECURITIES_PREFIX: &str = "Assets:Securities";

#[derive(clap::Parser, Debug)]
pub struct FreezeArgs {
    /// Where to write the reconciled journal CSV (financial data; gitignored).
    #[arg(long, default_value = "ledger/journal.csv")]
    pub journal: PathBuf,

    #[command(flatten)]
    pub build: BuildArgs,
}

/// A transaction's legs after inference and balancing.
#[derive(Debug)]
struct PostingRow {
    account: String,
    amount: Decimal,
    currency: Currency,
    tags: Option<String>,
}

fn is_placeholder(path: &str) -> bool { in_subtree(path, SECURITIES_PREFIX) }

/// Fills an inferred leg and balances the transaction, plugging a genuine
/// cross-currency transfer through `Equity:Conversions`. A single-currency
/// transaction that does not net to zero is a real error and stops the freeze.
///
/// A model leg's own `currency` is meaningful only when it carries an amount;
/// an inferred leg (`amount == None`) leaves it at the default, so its currency
/// is taken from the rest of the transaction.
fn balance_postings(
    postings: &[Posting],
    date: NaiveDate,
    tags: &Option<String>,
) -> Result<Vec<PostingRow>> {
    let inferred = postings.iter().filter(|p| p.amount.is_none()).count();
    if inferred > 1 {
        bail!("transaction on {date} has more than one inferred posting");
    }
    let currencies: BTreeSet<Currency> =
        postings.iter().filter(|p| p.amount.is_some()).map(|p| p.currency).collect();
    let mut residual: BTreeMap<Currency, Decimal> = BTreeMap::new();
    let mut rows: Vec<PostingRow> = Vec::new();

    for p in postings {
        let (amount, currency) = match p.amount {
            Some(amount) => (amount, p.currency),
            None => {
                let currency = match currencies.iter().collect::<Vec<_>>().as_slice() {
                    [only] => **only,
                    _ => bail!(
                        "cannot infer the balancing leg of a multi-currency transaction on {date}"
                    ),
                };
                let sum: Decimal = postings.iter().filter_map(|q| q.amount).sum();
                (-sum, currency)
            }
        };
        *residual.entry(currency).or_default() += amount;
        let placeholder = is_placeholder(&p.account).then(|| PLACEHOLDER_TAG.to_string());
        rows.push(PostingRow {
            account: p.account.clone(),
            amount,
            currency,
            tags: merge_tags(tags, placeholder),
        });
    }

    let unbalanced: Vec<(Currency, Decimal)> =
        residual.into_iter().filter(|(_, amount)| !amount.is_zero()).collect();
    match unbalanced.as_slice() {
        [] => {}
        [(currency, amount)] => {
            bail!("transaction on {date} does not balance: {amount} {currency} left over")
        }
        many => {
            for (currency, amount) in many {
                rows.push(PostingRow {
                    account: CONVERSIONS.to_string(),
                    amount: -amount,
                    currency: *currency,
                    tags: None,
                });
            }
        }
    }
    Ok(rows)
}

/// Joins transaction tags with an optional per-posting marker into the single
/// comma-separated string a posting carries.
fn merge_tags(txn_tags: &Option<String>, extra: Option<String>) -> Option<String> {
    match (txn_tags, extra) {
        (t, None) => t.clone(),
        (None, Some(e)) => Some(e),
        (Some(t), Some(e)) => Some(format!("{t},{e}")),
    }
}

/// Folds the importer's model plus the `manual.csv` transactions straight into
/// a [`journal::Journal`], returning it with the balance assertions to check it
/// against. Never re-decides where a posting goes; each transaction becomes one
/// `group`.
fn reconcile(
    model: &model::Model,
    manual: &[model::Transaction],
) -> Result<(journal::Journal, Vec<BalanceAssertion>)> {
    let mut postings: Vec<journal::Posting> = Vec::new();
    for (group, txn) in (0u64..).zip(model.transactions().chain(manual)) {
        let tags = (!txn.tags.is_empty()).then(|| txn.tags.join(","));
        let payee = (!txn.payee.is_empty()).then(|| txn.payee.clone());
        for leg in balance_postings(&txn.postings, txn.date, &tags)? {
            postings.push(journal::Posting {
                group,
                source: txn.source,
                date: txn.date,
                payee: payee.clone(),
                narration: txn.narration.clone(),
                external_ref: txn.external_ref.clone(),
                account: leg.account,
                amount: leg.amount,
                currency: leg.currency,
                tags: leg.tags,
            });
        }
    }

    // The loader keys dedup on (source, external_ref), so a journal with two
    // transactions under one key cannot be loaded at all. Refuse to write one.
    let mut keys: BTreeSet<(model::Source, &str)> = BTreeSet::new();
    for txn in model.transactions().chain(manual) {
        if let Some(external_ref) = &txn.external_ref {
            if !keys.insert((txn.source, external_ref)) {
                bail!("two transactions share the dedup key ({}, {external_ref})", txn.source);
            }
        }
    }

    let tiantian = model.asserts.iter().filter_map(|d| match d {
        model::Directive::Balance(b) => Some(BalanceAssertion {
            source: AssertionSource::Tiantian,
            account: b.account.clone(),
            currency: b.currency,
            period_start: None,
            opening: None,
            // Beancount asserts at the start of the day.
            period_end: b.date.pred_opt().expect("an assertion date has a day before"),
            closing: b.amount,
        }),
        _ => None,
    });
    let assertions: Vec<BalanceAssertion> =
        model.statements.iter().cloned().chain(tiantian).collect();

    // Every Beancount balance must be carried over: a statement's opening and
    // closing by its one assertion (which may hold back a possibly partial
    // last day), a 天天記帳 balance by its own.
    let carried: usize = assertions
        .iter()
        .map(|a| match a.source {
            AssertionSource::Statement => 2,
            AssertionSource::Tiantian | AssertionSource::Counted => 1,
        })
        .sum();
    if carried != model.balances().count() {
        bail!(
            "{} ledger balance assertions but {carried} carried into the journal",
            model.balances().count()
        );
    }
    Ok((journal::Journal { postings }, assertions))
}

/// The name a file is written under until it is verified.
fn staged(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".staged");
    path.with_file_name(name)
}

/// Balance figures asserted: a statement period has two.
fn figures(assertions: &[BalanceAssertion]) -> usize {
    assertions.iter().map(|a| 1 + usize::from(a.opening.is_some())).sum()
}

/// The subtree balance per currency at `cutoff` (start-of-day: strictly before,
/// matching Beancount's assertion semantics). `cutoff = None` folds in
/// everything; `subtree = false` restricts to the exact account.
fn subtree_balance(
    postings: &[journal::Posting],
    root: &str,
    cutoff: Option<NaiveDate>,
    subtree: bool,
) -> BTreeMap<Currency, Decimal> {
    let mut totals: BTreeMap<Currency, Decimal> = BTreeMap::new();
    for m in postings {
        let matches = if subtree { in_subtree(&m.account, root) } else { m.account == root };
        let before = cutoff.map(|c| m.date < c).unwrap_or(true);
        if matches && before {
            *totals.entry(m.currency).or_default() += m.amount;
        }
    }
    totals
}

/// One reconciliation failure.
#[derive(Debug)]
pub struct Mismatch {
    pub account: String,
    pub currency: Currency,
    pub expected: Decimal,
    pub actual: Decimal,
    /// The balance compared is the one at the end of this day.
    pub date: NaiveDate,
}

/// One asset account that would close negative.
#[derive(Debug)]
pub struct Negative {
    pub account: String,
    pub currency: Currency,
    pub amount: Decimal,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MISMATCH {} {} at end of {}: expected {}, journal has {}",
            self.account, self.currency, self.date, self.expected, self.actual
        )
    }
}

impl fmt::Display for Negative {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} = {}", self.account, self.currency, self.amount)
    }
}

/// What the freeze produced and whether the journal reconciles.
#[derive(Debug)]
pub struct Report {
    pub journal: PathBuf,
    pub accounts: usize,
    pub transactions: usize,
    pub postings: usize,
    pub openings: usize,
    pub placeholders: usize,
    pub conversions: usize,
    pub manual: usize,
    /// Balance figures checked: a statement period states two.
    pub figures_checked: usize,
    pub mismatches: Vec<Mismatch>,
    pub negatives: Vec<Negative>,
}

impl Report {
    /// Mirrors `store::check`: a journal no balance assertion vouches for is
    /// not a verified journal, whatever else reconciles.
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty() && self.negatives.is_empty() && self.figures_checked > 0
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ok() {
            writeln!(f, "froze reconciled history to {}", self.journal.display())?;
        } else {
            writeln!(
                f,
                "reconciliation FAILED — nothing written; {} still holds whatever the last \
                 verified freeze left, which may be stale",
                self.journal.display()
            )?;
        }
        writeln!(f, "  {} accounts", self.accounts)?;
        writeln!(f, "  {} transactions, {} postings", self.transactions, self.postings)?;
        writeln!(f, "  {} opening balances", self.openings)?;
        writeln!(f, "  {} cross-currency conversion legs via {CONVERSIONS}", self.conversions)?;
        writeln!(
            f,
            "  {} securities-placeholder postings tagged #{PLACEHOLDER_TAG}",
            self.placeholders
        )?;
        match self.figures_checked {
            0 => writeln!(f, "reconciliation: nothing vouches for this journal")?,
            n => writeln!(
                f,
                "reconciliation: {n} balance figures checked, {} failed",
                self.mismatches.len()
            )?,
        }
        for m in &self.mismatches {
            writeln!(f, "  {m}")?;
        }
        if self.negatives.is_empty() {
            writeln!(f, "no asset account closes negative")?;
        } else {
            writeln!(f, "NEGATIVE asset balances (opening balance missing?):")?;
            for n in &self.negatives {
                writeln!(f, "  {n}")?;
            }
        }
        if self.manual > 0 {
            writeln!(
                f,
                "note: {} manual.csv entries are in the journal (and SQLite) but NOT in the Fava \
                 ledger — reconcile cash by hand until Fava is retired",
                self.manual
            )?;
        }
        Ok(())
    }
}

/// Verifies the journal's rows reproduce every assertion and that no asset
/// account closes negative — except a split account, where negative just
/// means you owe them.
fn verify(
    journal: &journal::Journal,
    assertions: &[BalanceAssertion],
    chart: &Chart,
) -> (Vec<Mismatch>, Vec<Negative>) {
    let mut mismatches = Vec::new();
    for a in assertions {
        for (date, expected) in a.points() {
            let cutoff = date.succ_opt().expect("a day after");
            let totals = subtree_balance(&journal.postings, &a.account, Some(cutoff), true);
            let actual = totals.get(&a.currency).copied().unwrap_or_default();
            if actual != expected {
                mismatches.push(Mismatch {
                    account: a.account.clone(),
                    currency: a.currency,
                    expected,
                    actual,
                    date,
                });
            }
        }
    }

    // Every asset account's final balance must be non-negative, bar the
    // split accounts, which are payables when negative.
    let asset_accounts: BTreeSet<&str> = journal
        .postings
        .iter()
        .map(|m| m.account.as_str())
        .filter(|a| matches!(a.parse(), Ok(AccountType::Assets)))
        .filter(|a| !chart.is_split_account(a))
        .collect();
    let mut negatives = Vec::new();
    for account in asset_accounts {
        for (currency, amount) in subtree_balance(&journal.postings, account, None, false) {
            if amount.is_sign_negative() {
                negatives.push(Negative { account: account.to_string(), currency, amount });
            }
        }
    }
    (mismatches, negatives)
}

/// Runs the freeze: reconcile, write the journal, and verify it — keeping the
/// journal only if it reconciles.
pub fn run(args: &FreezeArgs) -> Result<Report> {
    let (model, summary) = ledger::build::assemble(&args.build)?;
    log::info!("freeze: assembled ledger model\n{summary}");

    let manual_path = args.build.ledger_dir.join("manual.csv");
    let manual = if manual_path.exists() { manual::load(&manual_path)? } else { Vec::new() };

    let (journal, assertions) = reconcile(&model, &manual)?;
    // Staged beside their final names, so a failed run never leaves an
    // unverified journal behind and the last verified pair survives it.
    let assertions_path = journal::assertions_path(&args.journal);
    let staged_journal = staged(&args.journal);
    let staged_assertions = staged(&assertions_path);
    journal::write(&staged_journal, &journal)?;
    journal::write_assertions(&staged_assertions, &assertions)?;

    // Trust the journal only after re-reading what was written and checking it.
    let written = journal::read(&staged_journal)?;
    let written_assertions = journal::read_assertions(&staged_assertions)?;
    let chart = Chart::load(args.build.ledger_dir.join("mapping.toml"))?;
    let (mismatches, negatives) = verify(&written, &written_assertions, &chart);

    let accounts: BTreeSet<&str> = written.postings.iter().map(|p| p.account.as_str()).collect();
    let transactions: BTreeSet<u64> = written.postings.iter().map(|p| p.group).collect();
    let report = Report {
        journal: args.journal.clone(),
        accounts: accounts.len(),
        transactions: transactions.len(),
        postings: written.postings.len(),
        openings: written.postings.iter().filter(|p| p.account == model::OPENING_EQUITY).count(),
        placeholders: written
            .postings
            .iter()
            .filter(|p| p.tags.as_deref().is_some_and(|t| t.contains(PLACEHOLDER_TAG)))
            .count(),
        conversions: written.postings.iter().filter(|p| p.account == CONVERSIONS).count(),
        manual: manual.len(),
        figures_checked: figures(&written_assertions),
        mismatches,
        negatives,
    };

    if report.ok() {
        std::fs::rename(&staged_assertions, &assertions_path)
            .with_context(|| format!("moving {} into place", assertions_path.display()))?;
        // Two renames cannot be one atomic step. If the second fails, drop the
        // assertions the first moved, and the staged journal with them, rather
        // than leave a pair that does not belong together: a missing file stops
        // the loader, a stale pairing would pass unnoticed. A crash here is the
        // same shape, and the load's check is what catches it.
        if let Err(e) = std::fs::rename(&staged_journal, &args.journal) {
            let orphaned = std::fs::remove_file(&assertions_path).is_err();
            let _ = std::fs::remove_file(&staged_journal);
            let journal = args.journal.display();
            let assertions = assertions_path.display();
            return Err(anyhow::Error::new(e).context(match orphaned {
                false => format!(
                    "moving {journal} into place; {assertions} was removed to keep the pair \
                     consistent — re-run freeze"
                ),
                true => format!(
                    "moving {journal} into place, and {assertions} could not be removed — delete \
                     it by hand before loading; it describes a journal that was never written"
                ),
            }));
        }
        log::info!("freeze: journal verified");
    } else {
        // Discard the staged pair; the last verified journal stays as it is.
        for path in [&staged_journal, &staged_assertions] {
            std::fs::remove_file(path)
                .with_context(|| format!("removing unverified {}", path.display()))?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
