//! What every bank import does once its statements are parsed: plan against
//! the ledger, insert, record the statements' figures, and report what does
//! not agree. Only a file it cannot use stops it.

use std::{collections::BTreeMap, fmt, path::PathBuf};

use anyhow::{bail, Context, Result};
use db::{
    assertions, check,
    import::{ensure_account, insert_deduped, InsertOutcome, Origin, Transaction},
    import_batch,
    import_run::{self, Run, Suggestion},
    SqliteConnection, SqlitePool,
};
use ledger::{
    accounts::Chart,
    labels::Labels,
    model::{Source, OPENING_EQUITY},
    names::statement_account,
    statements::bank::{Bank, Merged},
};

use super::plan::{self, Candidate, Gap, Plan};

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// Statement exports; each account's must chain on from one another.
    #[arg(long, num_args = 1.., required = true)]
    pub statements: Vec<PathBuf>,

    /// No default: until the Cutover this runs against scratch databases only.
    #[arg(long)]
    pub database_url: String,

    /// Directory holding mapping.toml.
    #[arg(long, default_value = "ledger")]
    pub ledger_dir: PathBuf,
}

#[derive(Debug)]
pub struct Report {
    pub bank: Bank,
    pub counts: plan::Counts,
    pub inserted: usize,
    pub batches: usize,
    /// Every transaction it touched, and what it suggests looking at.
    pub run: Run,
    pub gaps: Vec<Gap>,
    /// Closings this download states against a different one already recorded,
    /// which stands.
    pub stale: Vec<String>,
    pub check: check::CheckReport,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.counts;
        writeln!(f, "imported {} statements:", self.bank)?;
        writeln!(f, "  {} transactions inserted from {} files", self.inserted, self.batches)?;
        writeln!(f, "  {} openings created", c.openings)?;
        writeln!(
            f,
            "  {} lines new ({} matched, {} uncategorised)",
            c.new, c.matched, c.uncategorised
        )?;
        writeln!(f, "  {} lines already held, {} booked with their partner", c.known, c.covered)?;
        if c.verified > 0 {
            writeln!(f, "  {} unverified records verified by their lines, in place", c.verified)?;
        }
        if c.moved > 0 {
            writeln!(
                f,
                "  {} unverified records moved to the sibling account whose statement shows them",
                c.moved
            )?;
        }
        if c.deferred > 0 {
            writeln!(
                f,
                "  {} unverified records not on the statement yet, moved to the day after it",
                c.deferred
            )?;
        }
        writeln!(f, "  {} lines on or before the account's opening", c.predate_opening)?;
        write!(f, "{}", self.run)?;
        for gap in &self.gaps {
            writeln!(f, "  {gap}")?;
        }
        for stale in &self.stale {
            writeln!(f, "  {stale}")?;
        }
        write!(f, "{}", self.check)
    }
}

pub async fn run(bank: Bank, args: &Args) -> Result<Report> {
    let chart = Chart::load(args.ledger_dir.join("mapping.toml"))?;
    let merged = bank.load_merged(&args.statements)?;
    let pool = db::init_db(&args.database_url).await?;
    import_all(&pool, &chart, bank, &merged, &[]).await
}

/// [`import`] as one database transaction, committed if it succeeds.
pub async fn import_all(
    pool: &SqlitePool,
    chart: &Chart,
    bank: Bank,
    merged: &[Merged],
    candidates: &[Candidate],
) -> Result<Report> {
    let mut tx = pool.begin().await?;
    let report = import(&mut tx, chart, bank, merged, candidates).await?;
    tx.commit().await?;
    Ok(report)
}

/// Imports `bank`'s statements into the caller's transaction; commit only on
/// `Ok`.
pub async fn import(
    db: &mut SqliteConnection,
    chart: &Chart,
    bank: Bank,
    merged: &[Merged],
    candidates: &[Candidate],
) -> Result<Report> {
    if let Some(other) = merged.iter().find(|m| m.statement.bank != bank) {
        bail!("{} is a {} statement, not {bank}", other.paths[0].display(), other.statement.bank);
    }
    // An issued statement with no rows (an idle currency) only vouches for its
    // balance.
    let (merged, idle): (Vec<&Merged>, Vec<&Merged>) =
        merged.iter().partition(|m| !m.statement.lines.is_empty());
    let files: Vec<&PathBuf> = merged.iter().flat_map(|m| &m.paths).collect();

    let mut statements = Vec::with_capacity(merged.len());
    let mut first_file = 0;
    for Merged { statement, spans, .. } in merged {
        let account = statement_account(chart, &statement.account_no)?.to_string();
        let existing = import_batch::postings(db, &account, statement.currency).await?;
        let files: Vec<usize> = spans
            .iter()
            .enumerate()
            .flat_map(|(i, &n)| std::iter::repeat(first_file + i).take(n))
            .collect();
        first_file += spans.len();
        statements.push(plan::Statement { account, statement, files, existing });
    }
    let known = import_batch::refs(db, bank.ref_prefix()).await?;
    let Plan { transactions, mut counts, openings, verified, deferred, moved, suggestions, gaps } =
        plan::plan(chart, &statements, &known, candidates);
    let labels = Labels::from(chart);
    let mut run = Run::default();
    for (id, suggestion) in suggestions {
        run.suggest(id, suggestion);
    }
    for m in &moved {
        import_batch::move_leg(db, &labels, m.transaction_id, &m.from, &m.to, m.amount).await?;
    }
    for v in &verified {
        import_batch::verify(db, v).await?;
        run.touched(v.transaction_id);
        // Another bank's statement has yet to vouch for its other leg.
        if !v.unchecked.is_empty() {
            run.suggest(v.transaction_id, Suggestion::Waiting);
        }
    }
    for d in &deferred {
        import_batch::redate(db, d.transaction_id, d.date).await?;
    }

    let mut inserted = 0;
    let mut batches: BTreeMap<usize, i64> = BTreeMap::new();
    for (file, mut txn) in transactions {
        let batch = match batches.get(&file) {
            Some(id) => *id,
            None => {
                let id = import_batch::create(db, bank.source(), files[file]).await?;
                batches.insert(file, id);
                id
            }
        };
        txn.import_batch_id = Some(batch);
        match insert_deduped(db, &labels, &txn).await.context("inserting import")? {
            InsertOutcome::Inserted(id) => {
                inserted += 1;
                run.touched(id);
                if txn.postings.iter().any(|p| p.origin == Some(Origin::Fallback)) {
                    run.suggest(id, Suggestion::Uncategorised);
                }
                if txn.postings.iter().any(|p| p.account == *chart.institution.clearing) {
                    run.suggest(id, Suggestion::Waiting);
                }
            }
            InsertOutcome::Duplicate(_) => counts.known += 1,
        }
    }

    // An idle account the ledger does not hold yet opens on its statement's
    // balance; a zero one needs no posting.
    let mut idle_batches = 0;
    for m in &idle {
        let s = &m.statement;
        let account = statement_account(chart, &s.account_no)?;
        let held = import_batch::postings(db, account, s.currency).await?;
        if !held.is_empty() || s.opening_balance().is_zero() {
            continue;
        }
        let batch = import_batch::create(db, bank.source(), &m.paths[0]).await?;
        idle_batches += 1;
        let opening = Transaction {
            date: s.opening_date(),
            payee: Some("Opening balance".to_string()),
            narration: Some(s.account_kind.clone()),
            source: Source::Import,
            external_ref: Some(s.opening_ref(account)),
            import_batch_id: Some(batch),
            postings: vec![
                (&**account, s.opening_balance(), s.currency).into(),
                (OPENING_EQUITY, -s.opening_balance(), s.currency).into(),
            ],
        };
        if let InsertOutcome::Inserted(id) =
            insert_deduped(db, &labels, &opening).await.context("inserting an opening")?
        {
            run.touched(id);
            inserted += 1;
            counts.openings += 1;
        }
    }

    // An assertion already recorded for the same closing day stands:
    // a later download states a wider period for that day, which is the same
    // claim, and the balance chain above has already compared every day. One
    // disagreeing is reported, and the ledger is held to the one recorded.
    let recorded = assertions::load(db).await?;
    let mut stale = Vec::new();
    let mut figures = Vec::new();
    for (s, opening) in statements.iter().zip(openings) {
        figures.extend(s.statement.assertions(&s.account, opening));
    }
    for m in idle {
        let account = statement_account(chart, &m.statement.account_no)?;
        figures.extend(m.statement.assertions(account, m.statement.opening_date()));
    }
    for assertion in figures {
        let same_day = recorded.iter().find(|r| {
            (&r.account, r.currency, r.source, r.period_end)
                == (&assertion.account, assertion.currency, assertion.source, assertion.period_end)
        });
        match same_day {
            Some(other) if other.closing != assertion.closing => stale.push(format!(
                "this download closes {} on {}, but {other} was recorded as closing {}; one of \
                 the two is stale",
                assertion.closing, assertion.period_end, other.closing
            )),
            Some(_) => continue,
            // assertions::insert resolves the account by path.
            None => {
                ensure_account(db, &labels, &assertion.account).await?;
                assertions::insert(db, &assertion).await?
            }
        }
    }
    let check = check::check(db).await?;
    import_run::record(db, bank.source(), &run).await?;

    Ok(Report {
        bank,
        counts,
        inserted,
        batches: batches.len() + idle_batches,
        run,
        gaps,
        stale,
        check,
    })
}
