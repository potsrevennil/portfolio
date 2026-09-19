//! Freeze implementation: reconcile the assembled model plus `manual.csv` into
//! a journal, write it, and verify it before trusting it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
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
};

/// Where a genuine cross-currency transfer's per-currency leftovers are booked
/// so every currency still sums to zero without a cost or price column.
const CONVERSIONS: &str = "Equity:Conversions";

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

fn account_type(path: &str) -> Result<AccountType> {
    path.split(':')
        .next()
        .unwrap_or("")
        .parse()
        .map_err(|_| anyhow::anyhow!("{path:?} has no root"))
}

/// True when an account is or lies under `root` (Beancount subtree semantics).
fn in_subtree(account: &str, root: &str) -> bool {
    account == root || account.starts_with(&format!("{root}:"))
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
) -> Result<(journal::Journal, Vec<model::Balance>)> {
    let mut postings: Vec<journal::Posting> = Vec::new();
    for (group, txn) in (0u64..).zip(model.transactions().chain(manual)) {
        let tags = (!txn.tags.is_empty()).then(|| txn.tags.join(","));
        let payee = (!txn.payee.is_empty()).then(|| txn.payee.clone());
        for leg in balance_postings(&txn.postings, txn.date, &tags)? {
            postings.push(journal::Posting {
                group,
                source: txn.source.as_str().to_string(),
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

    let assertions = model
        .balances()
        .map(|b| model::Balance {
            date: b.date,
            account: b.account.clone(),
            amount: b.amount,
            currency: b.currency,
        })
        .collect();

    Ok((journal::Journal { postings }, assertions))
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
    pub date: NaiveDate,
}

/// One asset account that would close negative.
#[derive(Debug)]
pub struct Negative {
    pub account: String,
    pub currency: Currency,
    pub amount: Decimal,
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
    pub assertions_checked: usize,
    pub mismatches: Vec<Mismatch>,
    pub negatives: Vec<Negative>,
}

impl Report {
    pub fn ok(&self) -> bool { self.mismatches.is_empty() && self.negatives.is_empty() }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ok() {
            writeln!(f, "froze reconciled history to {}", self.journal.display())?;
        } else {
            writeln!(
                f,
                "reconciliation FAILED — journal not trusted, {} removed",
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
        writeln!(
            f,
            "reconciliation: {} balance assertions checked, {} failed",
            self.assertions_checked,
            self.mismatches.len()
        )?;
        for m in &self.mismatches {
            writeln!(
                f,
                "  MISMATCH {} {} @ {}: expected {}, journal has {}",
                m.account, m.currency, m.date, m.expected, m.actual
            )?;
        }
        if self.negatives.is_empty() {
            writeln!(f, "no asset account closes negative")?;
        } else {
            writeln!(f, "NEGATIVE asset balances (opening balance missing?):")?;
            for n in &self.negatives {
                writeln!(f, "  {} {} = {}", n.account, n.currency, n.amount)?;
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
    assertions: &[model::Balance],
    chart: &Chart,
) -> (Vec<Mismatch>, Vec<Negative>) {
    let mut mismatches = Vec::new();
    for a in assertions {
        let totals = subtree_balance(&journal.postings, &a.account, Some(a.date), true);
        let actual = totals.get(&a.currency).copied().unwrap_or_default();
        if actual != a.amount {
            mismatches.push(Mismatch {
                account: a.account.clone(),
                currency: a.currency,
                expected: a.amount,
                actual,
                date: a.date,
            });
        }
    }

    // Every asset account's final balance must be non-negative, bar the
    // split accounts, which are payables when negative.
    let asset_accounts: BTreeSet<&str> = journal
        .postings
        .iter()
        .map(|m| m.account.as_str())
        .filter(|a| matches!(account_type(a), Ok(AccountType::Assets)))
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
    journal::write(&args.journal, &journal)?;

    // Trust the journal only after re-reading what was written and checking it.
    let written = journal::read(&args.journal)?;
    let chart = Chart::load(args.build.ledger_dir.join("mapping.toml"))?;
    let (mismatches, negatives) = verify(&written, &assertions, &chart);

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
        assertions_checked: assertions.len(),
        mismatches,
        negatives,
    };

    if report.ok() {
        log::info!("freeze: journal verified");
    } else {
        // Never leave an untrusted journal on disk for the loader to pick up.
        std::fs::remove_file(&args.journal)
            .with_context(|| format!("removing untrusted journal {}", args.journal.display()))?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
