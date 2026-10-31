//! 天天記帳 exports made after the freeze: the records newer than the frozen
//! history, booked as freeze books them.
//!
//! A record on a statement account never adds a second copy of a bank line.
//! It relabels the uncategorised line already imported for it. If the
//! statements don't reach its day yet, it books tagged unverified, and the
//! next statement verifies it in place. A record the statements should
//! explain but don't books the same way, with a suggestion.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use db::{
    assertions::{self, AssertionSource},
    check,
    import::{insert_deduped, InsertOutcome, Origin, Posting, Transaction},
    import_batch::{self, HeldTransaction},
    import_run::{self, Run, Suggestion},
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
    /// Every transaction it touched, and what it suggests looking at.
    pub run: Run,
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
        write!(f, "{}", self.run)?;
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
    /// Stands for this line, relabelling its fallback posting if it still has
    /// one: a line a person categorised keeps their category.
    Pairs { transaction_id: i64, fallback_posting: Option<i64> },
    /// Books tagged unverified on this day.
    Waits(NaiveDate),
}

/// Imports the records newer than `frozen` into the caller's transaction;
/// commit only on `Ok`.
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
    let fallbacks: BTreeSet<&str> = [&chart.fallback.income, &chart.fallback.expense]
        .into_iter()
        .chain(chart.fallback.descriptions.values())
        .map(|a| a.as_ref())
        .collect();
    let on_statement = |i: usize| match &records[i].booking {
        Booking::OnStatement { leg, .. } => leg,
        _ => unreachable!("grouped as a statement leg"),
    };
    // What each statement account holds, read once.
    let mut held: HashMap<(String, Currency), Vec<HeldTransaction>> = HashMap::new();
    let mut fates: HashMap<usize, Fate> = HashMap::new();
    // Lines a record of this export stands for.
    let mut taken: HashSet<i64> = HashSet::new();
    let mut run = Run::default();
    // What it suggests for a record not in the ledger yet.
    let mut suggestions: HashMap<usize, Suggestion> = HashMap::new();
    // Records the statements vouch for with no line, and through when.
    let mut unexplained: Vec<(usize, NaiveDate)> = Vec::new();
    for ((account, currency), at) in &legs {
        let key = (account.clone(), *currency);
        let on = import_batch::transactions_on(db, account, *currency).await?;
        let on = held.entry(key).or_insert(on);
        let lines: Vec<(&HeldTransaction, i64)> = on
            .iter()
            .filter(|t| !stands_for_a_record(t))
            .filter_map(|t| uncategorised_line(t, account, &fallbacks))
            .collect();
        let fits = |&i: &usize, (t, _): &(&HeldTransaction, i64)| {
            line_fits(t, account, on_statement(i), records[i].date)
        };
        let pairing = one_to_one(at, &lines, fits);
        let apart = |l: usize, m: usize| (records[at[l]].date - lines[m].0.date).num_days().abs();
        let nearest = pairing.nearest(apart);
        for &(_, m) in &nearest {
            run.suggest(lines[m].0.id, Suggestion::PickedNearest);
        }
        for &(l, m) in pairing.pairs.iter().chain(&nearest) {
            let (t, fallback_posting) = lines[m];
            taken.insert(t.id);
            let fate =
                Fate::Pairs { transaction_id: t.id, fallback_posting: Some(fallback_posting) };
            fates.insert(at[l], fate);
        }
        let through = vouched(account, *currency);
        for &i in at {
            if fates.contains_key(&i) {
                continue;
            }
            let r = &records[i];
            match through {
                Some(through) if r.date <= through => {
                    if (through - r.date).num_days() < WINDOW_DAYS {
                        fates.insert(i, Fate::Waits(through.succ_opt().expect("a next day")));
                    } else {
                        unexplained.push((i, through));
                    }
                }
                _ => {
                    fates.insert(i, Fate::Waits(r.date));
                }
            }
        }
    }
    // The lines of its amount each may be all the same, no other record of
    // this export taking them: by account, its own first, then the siblings
    // the app records together with it.
    let mut maybe: Vec<(usize, NaiveDate, Vec<(String, i64)>)> = Vec::new();
    for (i, through) in unexplained {
        let leg = on_statement(i);
        let mut lines = Vec::new();
        for account in [leg.account.clone()].into_iter().chain(chart.pool_siblings(&leg.account)) {
            let key = (account.clone(), leg.currency);
            if !held.contains_key(&key) {
                let on = import_batch::transactions_on(db, &account, leg.currency).await?;
                held.insert(key.clone(), on);
            }
            lines.extend(
                held[&key]
                    .iter()
                    .filter(|t| {
                        !taken.contains(&t.id)
                            && !stands_for_a_record(t)
                            && line_fits(t, &account, leg, records[i].date)
                    })
                    .map(|t| (account.clone(), t.id)),
            );
        }
        maybe.push((i, through, lines));
    }
    // One line on its own account that a person already categorised, and no
    // other record's: it is that line, which keeps its category. Anything
    // else books on its own day, naming the siblings with a line it fits.
    let own = |i: usize, lines: &[(String, i64)]| -> Vec<i64> {
        let leg = on_statement(i);
        lines.iter().filter(|(a, _)| *a == leg.account).map(|(_, id)| *id).collect()
    };
    let claims =
        |line: i64| maybe.iter().filter(|(i, _, lines)| own(*i, lines).contains(&line)).count();
    for (i, through, lines) in &maybe {
        match own(*i, lines).as_slice() {
            [only] if claims(*only) == 1 => {
                fates.insert(*i, Fate::Pairs { transaction_id: *only, fallback_posting: None });
            }
            _ => {
                let own_account = &on_statement(*i).account;
                let siblings: BTreeSet<String> = lines
                    .iter()
                    .filter(|(a, _)| a != own_account)
                    .map(|(a, _)| a.clone())
                    .collect();
                fates.insert(*i, Fate::Waits(records[*i].date));
                suggestions.insert(*i, Suggestion::NotOnStatement {
                    through: *through,
                    siblings: siblings.into_iter().collect(),
                });
            }
        }
    }

    let labels = Labels::from(chart);
    let placeholder = placeholder_root(chart)?;
    let (mut income_expense_batch, mut transfers_batch): (Option<i64>, Option<i64>) = (None, None);
    let mut span: Option<(NaiveDate, NaiveDate)> = None;
    let is_fallback = |account: &str| fallbacks.contains(account);
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
                run.touched(transaction_id);
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
            InsertOutcome::Inserted(id) => {
                run.touched(id);
                if row.postings.iter().any(|p| is_fallback(&p.account)) {
                    run.suggest(id, Suggestion::Uncategorised);
                }
                match suggestions.remove(&i) {
                    Some(suggestion) => run.suggest(id, suggestion),
                    None if transaction.tags.iter().any(|t| t == UNVERIFIED_TAG) => {
                        run.suggest(id, Suggestion::Waiting)
                    }
                    None => {}
                }
            }
            InsertOutcome::Duplicate(_) => counts.known += 1,
        }
    }

    let check = check::with_counts(db, chart).await?;
    import_run::record(db, SOURCE, &run).await?;
    Ok(Report { counts, late, frozen_through: frozen.through, span, unmapped, run, check })
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

/// Whether bank line `t` could be `leg`'s, dated `date`: from a statement, of
/// its amount on `account`, within the window.
fn line_fits(t: &HeldTransaction, account: &str, leg: &StatementLeg, date: NaiveDate) -> bool {
    from_a_statement(t)
        && within_window(t.date, date)
        && t.postings
            .iter()
            .any(|p| p.account == account && p.amount == leg.amount && p.currency == leg.currency)
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
            posted_date: None,
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
