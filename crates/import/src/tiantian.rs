//! 天天記帳 exports made after the freeze: the records newer than the frozen
//! history, booked as freeze books them and gated like any import.
//!
//! A record on a statement account never adds a second copy of a bank line.
//! It relabels the uncategorised line already imported for it. If the
//! statements don't reach its day yet, it books tagged unverified, and the
//! next statement verifies it in place. A record the statements should
//! explain but don't stops the import, until a person says in the review
//! queue which line or account it is.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use db::{
    assertions::{self, AssertionSource},
    check,
    import::{insert_deduped, InsertOutcome, Origin, Posting, Transaction},
    import_batch::{self, HeldTransaction},
    stops::{self, Pick, Record, Refused, Stop},
    SqliteConnection, SqlitePool,
};
use ledger::{
    accounts::{in_subtree, Chart},
    corrected::{self, Frozen},
    daily::{self, Entry},
    freeze::{journal_rows, placeholder_root},
    interim::{self, Booked, Booking, StatementLeg},
    journal::UNVERIFIED_TAG,
    labels::Labels,
    model::{self, Source},
    statements::bank::Bank,
};
use ledger_types::currency::Currency;

use crate::pairing::{one_to_one, within_window, WINDOW_DAYS};

/// `import_batch.source` for these imports.
pub const SOURCE: &str = "tiantian";

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
    /// Records dated within the frozen history that it does not hold: entered
    /// in the app after the freeze read it, so only `corrected/` can take
    /// them.
    pub late: Vec<(String, NaiveDate)>,
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
        writeln!(f, "  {} already frozen (through {through}), not imported", c.frozen)?;
        if !self.late.is_empty() {
            writeln!(
                f,
                "  {} dated inside the frozen history but not in it — add them to corrected/ \
                 transactions.csv and re-freeze, or leave them out on purpose:",
                self.late.len()
            )?;
            for (id, date) in &self.late {
                writeln!(f, "    {date} {id}")?;
            }
        }
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
    let files = Files { income_expense: &args.income_expense, transfers: &args.transfers };
    import_all(&pool, &chart, &entries, &frozen, files).await
}

/// [`import`] as one database transaction, committed if it succeeds. What
/// stopped it is kept for the review queue, though nothing else is written;
/// the next import places the records as a person said there.
pub async fn import_all(
    pool: &SqlitePool,
    chart: &Chart,
    entries: &[Entry],
    frozen: &Frozen,
    files: Files<'_>,
) -> Result<Report> {
    let mut tx = pool.begin().await?;
    let outcome = import(&mut tx, chart, entries, frozen, files).await;
    stops::finish(pool, SOURCE, tx, outcome).await
}

/// Where the records came from, for `import_batch`.
#[derive(Clone, Copy)]
pub struct Files<'a> {
    pub income_expense: &'a Path,
    pub transfers: &'a Path,
}

/// A record's statement leg, and how it books.
enum Fate {
    /// Stands for this line, relabelling its fallback posting if it still
    /// has one: a line a person categorised keeps their category.
    Pairs { transaction_id: i64, fallback_posting: Option<i64> },
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
    // Both forms the ledger may already hold this record under: the
    // importer's namespaced ref, and the bare UUID the frozen journal uses.
    let imported = import_batch::refs(db, interim::REF_PREFIX).await?;
    let frozen_refs = import_batch::refs_of(db, Source::Tiantian).await?;
    let mut fresh: Vec<Entry> = Vec::new();
    let mut late: Vec<(String, NaiveDate)> = Vec::new();
    for entry in entries {
        let id = match entry {
            Entry::Flow { id, .. } | Entry::Transfer { id, .. } => id.as_str(),
        };
        if frozen.ids.contains(id) {
            counts.frozen += 1;
        } else if frozen.through.is_some_and(|through| entry.date() <= through) {
            late.push((id.to_string(), entry.date()));
        } else if imported.contains(&interim::external_ref(id)) || frozen_refs.contains(id) {
            counts.known += 1;
        } else {
            fresh.push(entry.clone());
        }
    }
    let interim::Books { mut records, unmapped } = interim::book(chart, &fresh)?;
    counts.empty = fresh.len() - records.len();

    // What a person said in the review queue about records an earlier run
    // could not place: a sibling account books it there, a line is it.
    let picks = stops::picks(db, SOURCE).await?;
    for r in &mut records {
        if let (Booking::OnStatement { leg, transaction }, Some(Pick::Account(to))) =
            (&mut r.booking, picks.get(&interim::external_ref(&r.id)))
        {
            if chart.pool_siblings(&leg.account).contains(to) {
                for p in transaction.postings.iter_mut().filter(|p| p.account == leg.account) {
                    p.account = to.clone();
                }
                leg.account = to.clone();
            }
        }
    }
    let fallbacks: BTreeSet<&str> = [&chart.fallback.income, &chart.fallback.expense]
        .into_iter()
        .chain(chart.fallback.descriptions.values())
        .map(|a| a.as_ref())
        .collect();
    let mut fates: HashMap<usize, Fate> = HashMap::new();
    // Lines a record already stands for in this run.
    let mut taken: HashSet<i64> = HashSet::new();
    for (i, r) in records.iter().enumerate() {
        let (Booking::OnStatement { leg, .. }, Some(Pick::Line(line))) =
            (&r.booking, picks.get(&interim::external_ref(&r.id)))
        else {
            continue;
        };
        // A pick is checked again: one that no longer fits is no pick.
        let fits = statement_lines(db, chart, &fallbacks, leg, r.date)
            .await?
            .into_iter()
            .find(|l| l.transaction_id == *line && !taken.contains(line));
        if let Some(l) = fits {
            taken.insert(l.transaction_id);
            let fate =
                Fate::Pairs { transaction_id: l.transaction_id, fallback_posting: l.fallback };
            fates.insert(i, fate);
        }
    }

    // Each statement leg pairs with a line already held, or waits for one.
    let mut legs: BTreeMap<(String, Currency), Vec<usize>> = BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        if let Booking::OnStatement { leg, .. } = &r.booking {
            if !fates.contains_key(&i) {
                legs.entry((leg.account.clone(), leg.currency)).or_default().push(i);
            }
        }
    }
    let recorded = assertions::load(db).await?;
    // The statements vouch through here; a record in their last days may be
    // booked on the next one, as the bank import defers it.
    let vouched = |account: &str, currency: Currency| {
        recorded
            .iter()
            .filter(|a| {
                a.source == AssertionSource::Statement
                    && a.currency == currency
                    && in_subtree(account, &a.account)
            })
            .map(|a| a.period_end)
            .max()
    };
    let leg_of = |i: usize| match &records[i].booking {
        Booking::OnStatement { leg, .. } => leg,
        _ => unreachable!("grouped as a statement leg"),
    };
    let record = |i: usize| {
        let leg = leg_of(i);
        let r = &records[i];
        let Booking::OnStatement { transaction, .. } = &r.booking else {
            unreachable!("grouped as a statement leg")
        };
        Record {
            external_ref: interim::external_ref(&r.id),
            date: r.date,
            account: leg.account.clone(),
            amount: leg.amount,
            currency: leg.currency,
            counter: transaction
                .postings
                .iter()
                .filter(|p| p.account != leg.account)
                .map(|p| p.account.clone())
                .collect(),
            narration: transaction.narration.clone(),
        }
    };
    let mut problems: Vec<Stop> = Vec::new();
    for ((account, currency), at) in &legs {
        let held = import_batch::transactions_on(db, account, *currency).await?;
        let lines: Vec<(&HeldTransaction, i64)> = held
            .iter()
            .filter(|t| !taken.contains(&t.id) && !stands_for_a_record(t))
            .filter_map(|t| uncategorised_line(t, account, &fallbacks))
            .collect();
        let fits = |&i: &usize, (t, _): &(&HeldTransaction, i64)| {
            let own = t.postings.iter().find(|p| p.account == *account).expect("on the account");
            own.amount == leg_of(i).amount && within_window(t.date, records[i].date)
        };
        let pairing = one_to_one(at, &lines, fits);
        for (l, many) in &pairing.ambiguous {
            let lines = many.iter().map(|&m| lines[m].0.id).collect();
            problems.push(Stop::RecordAmbiguous { record: record(at[*l]), lines });
        }
        for &(l, m) in &pairing.pairs {
            let (t, fallback_posting) = lines[m];
            let fate =
                Fate::Pairs { transaction_id: t.id, fallback_posting: Some(fallback_posting) };
            fates.insert(at[l], fate);
        }
        let through = vouched(account, *currency);
        let ambiguous: BTreeSet<usize> = pairing.ambiguous.iter().map(|(l, _)| *l).collect();
        for (l, &i) in at.iter().enumerate() {
            if fates.contains_key(&i) || ambiguous.contains(&l) {
                continue;
            }
            let r = &records[i];
            match through {
                Some(through) if r.date <= through => {
                    if (through - r.date).num_days() < WINDOW_DAYS {
                        fates.insert(i, Fate::Waits(through.succ_opt().expect("a next day")));
                    } else {
                        // What it may be all the same: a line of its amount a
                        // person already categorised, or one on a sibling
                        // account; or a sibling whose statement is yet to come.
                        let lines = statement_lines(db, chart, &fallbacks, leg_of(i), r.date)
                            .await?
                            .into_iter()
                            .map(|l| l.transaction_id)
                            .filter(|id| !taken.contains(id))
                            .collect();
                        let accounts = chart
                            .pool_siblings(account)
                            .into_iter()
                            .filter(|a| vouched(a, *currency).is_none_or(|t| t < r.date))
                            .collect();
                        problems.push(Stop::RecordUnexplained {
                            record: record(i),
                            through,
                            lines,
                            accounts,
                        });
                    }
                }
                _ => {
                    fates.insert(i, Fate::Waits(r.date));
                }
            }
        }
    }
    if !problems.is_empty() {
        return Err(Refused(problems).into());
    }

    let labels = Labels::from(chart);
    let placeholder = placeholder_root(chart)?;
    let (mut income_expense_batch, mut transfers_batch): (Option<i64>, Option<i64>) = (None, None);
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
                // The bank line's category now comes from the record.
                if let Some(fallback_posting) = fallback_posting {
                    let far: Vec<Posting> = far_legs(&transaction, &leg, placeholder)?
                        .into_iter()
                        .map(|p| p.with_origin(Origin::Tiantian))
                        .collect();
                    import_batch::replace_leg(db, &labels, fallback_posting, &far).await?;
                }
                if !transaction.narration.is_empty() {
                    import_batch::append_narration(db, transaction_id, &transaction.narration)
                        .await?;
                }
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
        let (batch, file) = match transfer {
            true => (&mut transfers_batch, files.transfers),
            false => (&mut income_expense_batch, files.income_expense),
        };
        let batch = match batch {
            Some(id) => *id,
            None => *batch.insert(import_batch::create(db, SOURCE, file).await?),
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
    Ok(Report { counts, late, frozen_through: frozen.through, span, unmapped, check })
}

/// Already relabelled or picked for a 天天記帳 record.
fn stands_for_a_record(t: &HeldTransaction) -> bool {
    t.external_ref.iter().chain(&t.aliases).any(|r| r.starts_with(interim::REF_PREFIX))
}

fn from_a_statement(t: &HeldTransaction) -> bool {
    t.external_ref
        .as_deref()
        .is_some_and(|r| Bank::ALL.iter().any(|b| r.starts_with(b.ref_prefix())))
}

/// A statement line a record could be, though no uncategorised line fits it.
struct LineFit {
    transaction_id: i64,
    /// Its fallback posting, if it is uncategorised still.
    fallback: Option<i64>,
}

/// Statement lines of `leg`'s amount within the window of `date`, on its
/// account or a sibling, that stand for no record yet.
async fn statement_lines(
    db: &mut SqliteConnection,
    chart: &Chart,
    fallbacks: &BTreeSet<&str>,
    leg: &StatementLeg,
    date: NaiveDate,
) -> Result<Vec<LineFit>> {
    let mut fits = Vec::new();
    for account in [leg.account.clone()].into_iter().chain(chart.pool_siblings(&leg.account)) {
        for t in import_batch::transactions_on(db, &account, leg.currency).await? {
            let own = t.postings.iter().any(|p| {
                p.account == account && p.amount == leg.amount && p.currency == leg.currency
            });
            if own
                && from_a_statement(&t)
                && !stands_for_a_record(&t)
                && within_window(t.date, date)
            {
                let fallback = uncategorised_line(&t, &account, fallbacks).map(|(_, id)| id);
                fits.push(LineFit { transaction_id: t.id, fallback });
            }
        }
    }
    Ok(fits)
}

/// A statement line imported with no record behind it: two legs, one on
/// `account`, the other on a fallback. Returns it with the fallback's posting.
fn uncategorised_line<'t>(
    t: &'t HeldTransaction,
    account: &str,
    fallbacks: &BTreeSet<&str>,
) -> Option<(&'t HeldTransaction, i64)> {
    match t.postings.as_slice() {
        [a, b] if from_a_statement(t) => [(a, b), (b, a)]
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
            origin: None,
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
