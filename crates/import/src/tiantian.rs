//! 天天記帳 exports made after the freeze: the records newer than the frozen
//! history, booked as freeze books them and gated like any import.
//!
//! A record on a statement account never adds a second copy of a bank line.
//! It relabels the uncategorised line already imported for it. If the
//! statements don't reach its day yet, it books tagged unverified, and the
//! next statement verifies it in place. A record the statements should
//! explain but don't stops the import.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use db::{
    assertions::{self, AssertionSource},
    check,
    import::{insert_deduped, InsertOutcome, Posting, Transaction},
    import_batch::{self, HeldTransaction},
    SqliteConnection,
};
use ledger::{
    accounts::{in_subtree, Chart},
    corrected::{self, Frozen},
    daily::{self, Entry},
    freeze::{journal_rows, placeholder_root},
    interim::{self, Booked, Booking, StatementLeg},
    journal::UNVERIFIED_TAG,
    labels::Labels,
    model,
    statements::bank::Bank,
};
use ledger_types::currency::Currency;

use crate::pairing::{one_to_one, within_window, WINDOW_DAYS};

/// `import_batch.source` for these imports.
const SOURCE: &str = "tiantian";

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// The 收支 export.
    #[arg(long)]
    pub income_expense: PathBuf,

    /// The 轉帳 export.
    #[arg(long)]
    pub transfers: PathBuf,

    /// The records the loaded history was frozen from: what it names, or is
    /// dated within, is not imported.
    #[arg(long, default_value = "corrected/transactions.csv")]
    pub frozen: PathBuf,

    /// No default: until the Cutover this runs against scratch databases only.
    #[arg(long)]
    pub database_url: String,

    /// Directory holding mapping.toml.
    #[arg(long, default_value = "ledger")]
    pub ledger_dir: PathBuf,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub records: usize,
    /// Named in the frozen records.
    pub frozen: usize,
    /// Dated within the frozen history but not in it: entered late.
    pub late: usize,
    pub known: usize,
    /// New, but zero on both sides.
    pub empty: usize,
    /// Booked as written: no statement account.
    pub standalone: usize,
    /// On a statement account, waiting for its line.
    pub unverified: usize,
    /// Relabelled the uncategorised line already imported for it.
    pub paired: usize,
    /// Between two statement accounts, whose statements book it.
    pub left_to_statements: usize,
}

#[derive(Debug)]
pub struct Report {
    pub counts: Counts,
    pub frozen_through: Option<NaiveDate>,
    /// First and last day of what was inserted or paired.
    pub span: Option<(NaiveDate, NaiveDate)>,
    pub unmapped: BTreeSet<String>,
    pub check: check::CheckReport,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.counts;
        writeln!(f, "imported 天天記帳 records: {} in the exports", c.records)?;
        let through = self.frozen_through.map(|d| d.to_string()).unwrap_or_default();
        writeln!(
            f,
            "  {} frozen, {} late (dated through {through}), not imported",
            c.frozen, c.late
        )?;
        writeln!(f, "  {} already held, {} moving nothing", c.known, c.empty)?;
        writeln!(
            f,
            "  {} inserted: {} as written, {} unverified on statement accounts",
            c.standalone + c.unverified,
            c.standalone,
            c.unverified
        )?;
        writeln!(f, "  {} paired with bank lines already imported", c.paired)?;
        writeln!(
            f,
            "  {} transfers between statement accounts, left to them",
            c.left_to_statements
        )?;
        if let Some((first, last)) = self.span {
            writeln!(f, "  dated {first}..={last}")?;
        }
        if !self.unmapped.is_empty() {
            let names: Vec<&str> = self.unmapped.iter().map(String::as_str).collect();
            writeln!(f, "  unmapped labels, booked to a fallback: {}", names.join(", "))?;
        }
        write!(f, "{}", self.check)
    }
}

pub async fn run(args: &Args) -> Result<Report> {
    let chart = Chart::load(args.ledger_dir.join("mapping.toml"))?;
    let entries = daily::load_entries(&args.income_expense, &args.transfers)?;
    let frozen = corrected::frozen(&args.frozen)?;
    let pool = db::init_db(&args.database_url).await?;
    let mut tx = pool.begin().await?;
    let files = Files { income_expense: &args.income_expense, transfers: &args.transfers };
    let report = import(&mut tx, &chart, &entries, &frozen, files).await?;
    tx.commit().await?;
    Ok(report)
}

/// Where the records came from, for `import_batch`.
#[derive(Clone, Copy)]
pub struct Files<'a> {
    pub income_expense: &'a Path,
    pub transfers: &'a Path,
}

/// A record's statement leg, and how it books.
enum Fate {
    /// Relabels this uncategorised line's fallback posting.
    Pairs { transaction_id: i64, fallback_posting: i64 },
    /// Books tagged unverified on this day.
    Waits(NaiveDate),
}

/// Imports the records newer than `frozen` into the caller's transaction and
/// gates it; commit only on `Ok`.
pub async fn import(
    db: &mut SqliteConnection,
    chart: &Chart,
    entries: &[Entry],
    frozen: &Frozen,
    files: Files<'_>,
) -> Result<Report> {
    let mut counts = Counts { records: entries.len(), ..Counts::default() };
    let known = import_batch::refs(db, "").await?;
    let mut fresh: Vec<Entry> = Vec::new();
    for entry in entries {
        let id = match entry {
            Entry::Flow { id, .. } | Entry::Transfer { id, .. } => id.as_str(),
        };
        if frozen.ids.contains(id) {
            counts.frozen += 1;
        } else if frozen.through.is_some_and(|through| entry.date() <= through) {
            counts.late += 1;
        } else if known.contains(&interim::external_ref(id)) || known.contains(id) {
            counts.known += 1;
        } else {
            fresh.push(entry.clone());
        }
    }
    let interim::Books { records, unmapped } = interim::book(chart, &fresh)?;
    counts.empty = fresh.len() - records.len();

    // Each statement leg pairs with a line already held, or waits for one.
    let mut legs: BTreeMap<(String, Currency), Vec<usize>> = BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        if let Booking::OnStatement { leg, .. } = &r.booking {
            legs.entry((leg.account.clone(), leg.currency)).or_default().push(i);
        }
    }
    let recorded = assertions::load(db).await?;
    let fallbacks: BTreeSet<&str> = [&chart.fallback.income, &chart.fallback.expense]
        .into_iter()
        .chain(chart.fallback.descriptions.values())
        .map(|a| a.as_ref())
        .collect();
    let mut fates: HashMap<usize, Fate> = HashMap::new();
    let mut problems: Vec<String> = Vec::new();
    for ((account, currency), at) in &legs {
        let held = import_batch::transactions_on(db, account, *currency).await?;
        let lines: Vec<(&HeldTransaction, i64)> =
            held.iter().filter_map(|t| uncategorised_line(t, account, &fallbacks)).collect();
        let leg_of = |i: usize| match &records[i].booking {
            Booking::OnStatement { leg, .. } => leg,
            _ => unreachable!("grouped as a statement leg"),
        };
        let fits = |&i: &usize, (t, _): &(&HeldTransaction, i64)| {
            let own = t.postings.iter().find(|p| p.account == *account).expect("on the account");
            own.amount == leg_of(i).amount && within_window(t.date, records[i].date)
        };
        let pairing = one_to_one(at, &lines, fits);
        for (l, many) in &pairing.ambiguous {
            let r = &records[at[*l]];
            let days: Vec<String> = many.iter().map(|&m| lines[m].0.date.to_string()).collect();
            problems.push(format!(
                "{} {} {} matches {} bank lines within {WINDOW_DAYS} days ({})",
                r.id,
                r.date,
                leg_of(at[*l]).amount,
                many.len(),
                days.join(", ")
            ));
        }
        for &(l, m) in &pairing.pairs {
            let (t, fallback_posting) = lines[m];
            fates.insert(at[l], Fate::Pairs { transaction_id: t.id, fallback_posting });
        }
        // The statements vouch through here; a record in their last days may
        // be booked on the next one, as the bank import defers it.
        let vouched = recorded
            .iter()
            .filter(|a| {
                a.source == AssertionSource::Statement
                    && a.currency == *currency
                    && in_subtree(account, &a.account)
            })
            .map(|a| a.period_end)
            .max();
        let ambiguous: BTreeSet<usize> = pairing.ambiguous.iter().map(|(l, _)| *l).collect();
        for (l, &i) in at.iter().enumerate() {
            if fates.contains_key(&i) || ambiguous.contains(&l) {
                continue;
            }
            let r = &records[i];
            match vouched {
                Some(through) if r.date <= through => {
                    if (through - r.date).num_days() < WINDOW_DAYS {
                        fates.insert(i, Fate::Waits(through.succ_opt().expect("a next day")));
                    } else {
                        problems.push(format!(
                            "{} {} {} on {account}: the statements through {through} show no line \
                             for it",
                            r.id,
                            r.date,
                            leg_of(i).amount
                        ));
                    }
                }
                _ => {
                    fates.insert(i, Fate::Waits(r.date));
                }
            }
        }
    }
    if !problems.is_empty() {
        bail!(
            "records on statement accounts need review; fix them in 天天記帳 and export again. \
             Nothing was imported.\n  {}",
            problems.join("\n  ")
        );
    }

    let labels = Labels::from(chart);
    let placeholder = placeholder_root(chart)?;
    let mut batches: HashMap<bool, i64> = HashMap::new();
    let mut span: Option<(NaiveDate, NaiveDate)> = None;
    for (i, Booked { date, transfer, booking, .. }) in records.into_iter().enumerate() {
        let (mut transaction, leg) = match booking {
            Booking::LeftToStatements => {
                counts.left_to_statements += 1;
                continue;
            }
            Booking::Standalone(t) => {
                counts.standalone += 1;
                (t, None)
            }
            Booking::OnStatement { leg, transaction } => (transaction, Some(leg)),
        };
        span = Some(span.map_or((date, date), |(a, b)| (a.min(date), b.max(date))));
        match (leg, fates.remove(&i)) {
            (Some(leg), Some(Fate::Pairs { transaction_id, fallback_posting })) => {
                let external_ref = transaction.external_ref.clone().expect("booked with its ref");
                let far = far_legs(&transaction, &leg, placeholder)?;
                import_batch::replace_leg(db, &labels, fallback_posting, &far).await?;
                import_batch::add_ref(db, transaction_id, &external_ref).await?;
                counts.paired += 1;
                continue;
            }
            (Some(_), Some(Fate::Waits(day))) => {
                transaction.tags.push(UNVERIFIED_TAG.to_string());
                transaction.date = day;
                counts.unverified += 1;
            }
            (Some(_), None) => unreachable!("every statement leg has a fate"),
            (None, _) => {}
        }
        let batch = match batches.get(&transfer) {
            Some(id) => *id,
            None => {
                let file = if transfer { files.transfers } else { files.income_expense };
                let id = import_batch::create(db, SOURCE, file).await?;
                batches.insert(transfer, id);
                id
            }
        };
        let row = to_row(&transaction, Some(batch), placeholder)?;
        match insert_deduped(db, &labels, &row).await.context("inserting a record")? {
            InsertOutcome::Inserted(_) => {}
            InsertOutcome::Duplicate(_) => bail!(
                "{} was planned as new but is already held",
                row.external_ref.unwrap_or_default()
            ),
        }
    }

    let check = check::gate_with_counts(db, chart).await?;
    Ok(Report { counts, frozen_through: frozen.through, span, unmapped, check })
}

/// A statement line imported with no record behind it: two legs, one on
/// `account`, the other on a fallback. Returns it with the fallback's posting.
fn uncategorised_line<'t>(
    t: &'t HeldTransaction,
    account: &str,
    fallbacks: &BTreeSet<&str>,
) -> Option<(&'t HeldTransaction, i64)> {
    let from_a_statement = t
        .external_ref
        .as_deref()
        .is_some_and(|r| Bank::ALL.iter().any(|b| r.starts_with(b.ref_prefix())));
    match t.postings.as_slice() {
        [a, b] if from_a_statement => [(a, b), (b, a)]
            .into_iter()
            .find(|(own, other)| {
                own.account == account && fallbacks.contains(other.account.as_str())
            })
            .map(|(_, other)| (t, other.id)),
        _ => None,
    }
}

/// The record's legs other than its statement leg: what replaces a line's
/// fallback.
fn far_legs(
    transaction: &model::Transaction,
    leg: &StatementLeg,
    placeholder: &str,
) -> Result<Vec<Posting>> {
    let mut rows = to_row(transaction, None, placeholder)?.postings;
    let own = rows
        .iter()
        .position(|p| {
            (&p.account, p.amount, p.currency) == (&leg.account, leg.amount, leg.currency)
        })
        .context("a record without its statement leg")?;
    rows.remove(own);
    Ok(rows)
}

/// The row freeze would write for `transaction`, as an import.
fn to_row(
    transaction: &model::Transaction,
    import_batch_id: Option<i64>,
    placeholder: &str,
) -> Result<Transaction> {
    let postings = journal_rows(transaction, 0, placeholder)?
        .into_iter()
        .map(|p| Posting {
            account: p.account,
            amount: p.amount,
            currency: p.currency,
            tags: p.tags,
        })
        .collect();
    Ok(Transaction {
        date: transaction.date,
        payee: (!transaction.payee.is_empty()).then(|| transaction.payee.clone()),
        narration: (!transaction.narration.is_empty()).then(|| transaction.narration.clone()),
        source: transaction.source,
        external_ref: transaction.external_ref.clone(),
        import_batch_id,
        postings,
    })
}
