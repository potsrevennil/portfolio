//! Imports broker statements (IB, Firstrade, Cathay securities) into the
//! broker sub-ledger, with each statement's balances as holding assertions.
//!
//! ```text
//! cargo run -- import-broker --broker ib --database-url sqlite:scratch.db \
//!   --statements raw/ib/*.csv
//! ```

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use db::{
    broker::{self, NewRecord},
    check,
    holdings::{self, HoldingAssertion},
    import::ensure_account,
    import_batch, SqliteConnection,
};
use ledger::{accounts::Chart, labels::Labels, names::statement_account};
use ledger_types::assertion::AssertionSource;
use portfolio::{
    broker::{
        cathay, firstrade, ib, BrokerRecord, BrokerStatement, Commodity, Holdings, RecordKind,
    },
    securities::Securities,
};
use rust_decimal::Decimal;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Ib,
    Firstrade,
    CathaySecurities,
}

impl Source {
    fn prefix(self) -> &'static str {
        match self {
            Source::Ib => ib::REF_PREFIX,
            Source::Firstrade => firstrade::REF_PREFIX,
            Source::CathaySecurities => cathay::REF_PREFIX,
        }
    }

    fn batch_source(self) -> &'static str { self.prefix().trim_end_matches(':') }
}

#[derive(clap::Parser, Debug)]
pub struct Args {
    #[arg(long, value_enum)]
    pub broker: Source,

    /// Statement files: IB activity CSVs, Firstrade monthly PDFs or Cathay
    /// securities CSVs.
    #[arg(long, num_args = 1.., required = true)]
    pub statements: Vec<PathBuf>,

    /// No default: until the Cutover this runs against scratch databases only.
    #[arg(long)]
    pub database_url: String,

    /// Directory holding mapping.toml.
    #[arg(long, default_value = "ledger")]
    pub ledger_dir: PathBuf,

    /// Security names → tickers, for exports that name securities.
    #[arg(long, default_value = Securities::PATH)]
    pub securities: PathBuf,

    /// The ledger account, for exports that state no account number (Cathay
    /// securities); otherwise institution.accounts maps the statement's.
    #[arg(long)]
    pub account: Option<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub statements: usize,
    /// Same-period downloads set aside for a newer one.
    pub superseded: Vec<PathBuf>,
    pub inserted: usize,
    pub known: usize,
    pub openings: usize,
    pub assertions: usize,
    /// Trades stored without an execution date.
    pub undated_trades: usize,
    pub check: check::CheckReport,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "imported {} broker statements:", self.statements)?;
        for p in &self.superseded {
            writeln!(f, "  skipped {} (a newer download covers the same period)", p.display())?;
        }
        writeln!(f, "  {} records inserted, {} already held", self.inserted, self.known)?;
        writeln!(f, "  {} opening records", self.openings)?;
        writeln!(f, "  {} holding figures recorded", self.assertions)?;
        if self.undated_trades > 0 {
            writeln!(f, "  {} trades carry only a settlement date", self.undated_trades)?;
        }
        write!(f, "{}", self.check)
    }
}

pub async fn run(args: &Args) -> Result<Report> {
    let chart = Chart::load(args.ledger_dir.join("mapping.toml"))?;
    let names = match args.broker {
        Source::Ib => Default::default(),
        _ => Securities::load(&args.securities.to_string_lossy())?.symbols,
    };
    let (statements, superseded) = read(args.broker, &args.statements, &names)?;
    let pool = db::init_db(&args.database_url).await?;
    let mut tx = pool.begin().await?;
    let mut report =
        import(&mut tx, &chart, args.broker, &statements, args.account.as_deref()).await?;
    tx.commit().await?;
    report.superseded = superseded;
    Ok(report)
}

/// Statements with their files, oldest period first, and the files set aside.
pub type Read = (Vec<(PathBuf, BrokerStatement)>, Vec<PathBuf>);

/// Parses `paths`. Of several IB downloads for one period only the newest is
/// kept: they differ only in when they were generated.
pub fn read(source: Source, paths: &[PathBuf], names: &HashMap<String, String>) -> Result<Read> {
    let text = |p: &Path| std::fs::read_to_string(p).with_context(|| format!("{}", p.display()));
    let mut parsed: Vec<(PathBuf, BrokerStatement)> = match source {
        Source::Ib => paths
            .iter()
            .map(|p| {
                Ok((p.clone(), ib::parse(&text(p)?).with_context(|| p.display().to_string())?))
            })
            .collect::<Result<_>>()?,
        Source::Firstrade => {
            let texts = paths.iter().map(|p| firstrade::pdf_text(p)).collect::<Result<Vec<_>>>()?;
            paths.iter().cloned().zip(firstrade::parse_all(&texts, names)?).collect()
        }
        Source::CathaySecurities => paths
            .iter()
            .map(|p| {
                Ok((
                    p.clone(),
                    cathay::parse(&text(p)?, names).with_context(|| p.display().to_string())?,
                ))
            })
            .collect::<Result<_>>()?,
    };

    let mut newest: BTreeMap<(String, NaiveDate, NaiveDate), usize> = BTreeMap::new();
    for (i, (_, s)) in parsed.iter().enumerate() {
        let period = (s.account.clone(), s.period_start, s.period_end);
        match newest.get(&period) {
            Some(&j) if parsed[j].1.generated >= s.generated => {}
            _ => {
                newest.insert(period, i);
            }
        }
    }
    let keep: HashSet<usize> = newest.into_values().collect();
    let mut superseded = Vec::new();
    let mut kept = Vec::new();
    for (i, entry) in parsed.drain(..).enumerate() {
        if keep.contains(&i) {
            kept.push(entry);
        } else {
            superseded.push(entry.0);
        }
    }
    kept.sort_by_key(|(_, s)| (s.period_start, s.period_end));
    Ok((kept, superseded))
}

/// Imports into the caller's transaction and gates it; commit only on `Ok`.
pub async fn import(
    db: &mut SqliteConnection,
    chart: &Chart,
    source: Source,
    statements: &[(PathBuf, BrokerStatement)],
    account: Option<&str>,
) -> Result<Report> {
    let labels = Labels::from(chart);
    let mut known = broker::refs(db, source.prefix()).await?;
    let stored = broker::load(db).await?;
    let mut report = Report { statements: statements.len(), ..Default::default() };
    // Per ledger account: every record so far, for the symbols it has touched.
    let mut held: BTreeMap<String, Vec<BrokerRecord>> = BTreeMap::new();

    for (path, s) in statements {
        let ledger_account = match (account, s.account.as_str()) {
            (Some(a), _) => a.to_string(),
            (None, "") => bail!("{} states no account number; pass --account", path.display()),
            (None, no) => statement_account(chart, no)?.to_string(),
        };
        ensure_account(db, &labels, &ledger_account).await?;
        let so_far = held
            .entry(ledger_account.clone())
            .or_insert_with(|| stored.get(&ledger_account).cloned().unwrap_or_default());

        let openings = match (&s.opening, so_far.is_empty()) {
            (Some(_), true) => s.opening_records(),
            (None, true) if s.closing.is_some() => bail!(
                "{} opens holding securities it doesn't list; import the statement before it first",
                path.display()
            ),
            _ => Vec::new(),
        };
        report.openings += openings.len();

        // A file that adds nothing leaves no batch behind.
        let mut batch = None;
        for r in openings.iter().chain(&s.records) {
            let external_ref = format!("{}{}:{}", source.prefix(), s.account, r.key);
            if !known.insert(external_ref.clone()) {
                report.known += 1;
                continue;
            }
            let id = match batch {
                Some(id) => id,
                None => *batch.insert(import_batch::create(db, source.batch_source(), path).await?),
            };
            let new = NewRecord {
                account: ledger_account.clone(),
                external_ref,
                import_batch_id: Some(id),
                record: r.clone(),
            };
            broker::insert(db, &new).await?;
            report.inserted += 1;
            let trade = matches!(r.kind, RecordKind::Buy | RecordKind::Sell);
            if trade && r.trade_date.is_none() {
                report.undated_trades += 1;
            }
            so_far.push(r.clone());
        }

        let points = [
            s.opening.as_ref().map(|h| (s.opening_date(), h)),
            s.closing.as_ref().map(|h| (s.period_end, h)),
        ];
        for (as_of, stated) in points.into_iter().flatten() {
            for (commodity, quantity) in figures(stated, so_far, as_of) {
                let a = HoldingAssertion {
                    source: AssertionSource::Statement,
                    account: ledger_account.clone(),
                    as_of,
                    commodity,
                    quantity,
                };
                if holdings::insert(db, &a).await? {
                    report.assertions += 1;
                }
            }
        }
    }
    report.check = check::gate(db).await?;
    Ok(report)
}

/// What a statement vouches for: each currency and position it lists, and
/// zero for every security the account touched by then that it doesn't list.
fn figures(
    stated: &Holdings,
    records: &[BrokerRecord],
    as_of: NaiveDate,
) -> Vec<(Commodity, Decimal)> {
    let touched: BTreeSet<&String> = records
        .iter()
        .filter(|r| r.settle_date <= as_of && !r.quantity.is_zero())
        .filter_map(|r| r.symbol.as_ref())
        .collect();
    let unlisted = touched
        .into_iter()
        .filter(|s| !stated.positions.contains_key(*s))
        .map(|s| (Commodity::Security(s.clone()), Decimal::ZERO));
    stated.iter().chain(unlisted).collect()
}
