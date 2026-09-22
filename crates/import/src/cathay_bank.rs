//! Imports Cathay bank downloads (活存, 投資, koko, 外幣) straight into SQLite.
//!
//! ```text
//! cargo run -- import-cathay-bank --database-url sqlite:scratch.db \
//!   --statements raw/cathay-bank/*/*.csv
//! ```

use std::{collections::BTreeMap, fmt, path::PathBuf};

use anyhow::{bail, Context, Result};
use ledger::{
    accounts::Chart,
    labels::Labels,
    names::statement_account,
    statements::cathay::{self, Merged},
};
use sqlx::SqliteConnection;
use store::{
    assertions, check,
    import::{ensure_account, insert_deduped, InsertOutcome},
    import_batch,
};

use super::plan::{self, Candidate, Plan};

pub const SOURCE: &str = "cathay-bank";

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
    pub counts: plan::Counts,
    pub inserted: usize,
    pub batches: usize,
    pub check: check::CheckReport,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.counts;
        writeln!(f, "imported Cathay bank statements:")?;
        writeln!(f, "  {} transactions inserted from {} files", self.inserted, self.batches)?;
        writeln!(f, "  {} openings created", c.openings)?;
        writeln!(
            f,
            "  {} lines new ({} matched, {} uncategorised)",
            c.new, c.matched, c.uncategorised
        )?;
        writeln!(f, "  {} lines already held, {} booked with their partner", c.known, c.covered)?;
        writeln!(f, "  {} lines on or before the account's opening", c.predate_opening)?;
        write!(f, "{}", self.check)
    }
}

pub async fn run(args: &Args) -> Result<Report> {
    let chart = Chart::load(args.ledger_dir.join("mapping.toml"))?;
    let pool = db::init_db(&args.database_url).await?;
    let mut tx = pool.begin().await?;
    let report = import(&mut tx, &chart, &args.statements, &[]).await?;
    tx.commit().await?;
    Ok(report)
}

/// Imports into the caller's transaction and gates it; commit only on `Ok`.
pub async fn import(
    db: &mut SqliteConnection,
    chart: &Chart,
    paths: &[PathBuf],
    candidates: &[Candidate],
) -> Result<Report> {
    let merged = cathay::load_merged(paths)?;
    let files: Vec<&PathBuf> = merged.iter().flat_map(|m| &m.paths).collect();

    let mut statements = Vec::with_capacity(merged.len());
    let mut first_file = 0;
    for Merged { statement, spans, .. } in &merged {
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
    let known = import_batch::refs(db, cathay::REF_PREFIX).await?;
    let Plan { transactions, counts, openings } =
        plan::plan(chart, &statements, &known, candidates)?;

    let labels = Labels::from(chart);
    let inserted = transactions.len();
    let mut batches: BTreeMap<usize, i64> = BTreeMap::new();
    for (file, mut txn) in transactions {
        let batch = match batches.get(&file) {
            Some(id) => *id,
            None => {
                let id = import_batch::create(db, SOURCE, files[file]).await?;
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

    // An assertion already recorded for the same closing day stands:
    // a later download states a wider period for that day, which is the same
    // claim, and the balance chain above has already compared every day.
    let recorded = assertions::load(db).await?;
    for (s, opening) in statements.iter().zip(openings) {
        let Some(assertion) = s.statement.assertion(&s.account, opening) else { continue };
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
                ensure_account(db, &labels, &s.account).await?;
                assertions::insert(db, &assertion).await?
            }
        }
    }
    let check = check::gate(db).await?;

    Ok(Report { counts, inserted, batches: batches.len(), check })
}
