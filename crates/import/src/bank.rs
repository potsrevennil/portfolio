//! What every bank import does once its statements are parsed: plan against
//! the ledger, insert, record the statements' figures, and gate.

use std::{collections::BTreeMap, fmt, path::PathBuf};

use anyhow::{bail, Context, Result};
use db::{
    assertions, check,
    import::{ensure_account, insert_deduped, InsertOutcome, Transaction},
    import_batch, SqliteConnection,
};
use ledger::{
    accounts::Chart,
    labels::Labels,
    model::{Source, OPENING_EQUITY},
    names::statement_account,
    statements::bank::{Bank, Merged},
};

use super::plan::{self, Candidate, Plan};

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
        if c.deferred > 0 {
            writeln!(
                f,
                "  {} unverified records not on the statement yet, moved to the day after it",
                c.deferred
            )?;
        }
        writeln!(f, "  {} lines on or before the account's opening", c.predate_opening)?;
        write!(f, "{}", self.check)
    }
}

pub async fn run(bank: Bank, args: &Args) -> Result<Report> {
    let chart = Chart::load(args.ledger_dir.join("mapping.toml"))?;
    let merged = bank.load_merged(&args.statements)?;
    let pool = db::init_db(&args.database_url).await?;
    let mut tx = pool.begin().await?;
    let report = import(&mut tx, &chart, bank, &merged, &[]).await?;
    tx.commit().await?;
    Ok(report)
}

/// Imports `bank`'s statements into the caller's transaction and gates it;
/// commit only on `Ok`.
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
    let Plan { transactions, mut counts, openings, verified, deferred } =
        plan::plan(chart, &statements, &known, candidates)?;
    for v in &verified {
        import_batch::verify(db, v).await?;
    }
    for d in &deferred {
        import_batch::redate(db, d.transaction_id, d.date).await?;
    }

    let labels = Labels::from(chart);
    let mut inserted = transactions.len();
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
            InsertOutcome::Inserted(_) => {}
            InsertOutcome::Duplicate(_) => bail!(
                "{} was planned as new but is already held",
                txn.external_ref.unwrap_or_default()
            ),
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
        match insert_deduped(db, &labels, &opening).await.context("inserting an opening")? {
            InsertOutcome::Inserted(_) => {}
            InsertOutcome::Duplicate(_) => {
                bail!("{} holds no postings but has an opening", &**account)
            }
        }
        inserted += 1;
        counts.openings += 1;
    }

    // An assertion already recorded for the same closing day stands:
    // a later download states a wider period for that day, which is the same
    // claim, and the balance chain above has already compared every day.
    let recorded = assertions::load(db).await?;
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
            Some(other) if other.closing != assertion.closing => bail!(
                "this download closes {} on {}, but {other} was recorded as closing {}; one of \
                 the two is stale — re-freeze, or drop the assertion that is",
                assertion.closing,
                assertion.period_end,
                other.closing
            ),
            Some(_) => continue,
            // assertions::insert resolves the account by path.
            None => {
                ensure_account(db, &labels, &assertion.account).await?;
                assertions::insert(db, &assertion).await?
            }
        }
    }
    let check = check::gate(db).await?;

    Ok(Report { bank, counts, inserted, batches: batches.len() + idle_batches, check })
}
