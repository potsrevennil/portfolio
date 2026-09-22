//! Command-line surface: argument structs and dispatch, nothing else.
//!
//! Each command's work lives in the module that owns it, so this file says what
//! the commands are and not how any of them is carried out.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use db::{quotes::StockPriceStore, splits::StockSplitStore};
use portfolio::{calculate, record};
use prices::PriceService;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    calculate_args: calculate::Args,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Initialize and process broker data
    Init(calculate::Init),
    Record(Record),
    /// Build a Beancount ledger from downloaded statements
    Ledger(ledger::Args),
    /// Freeze reconciled history to a journal CSV (one-time / audit-time)
    Freeze(ledger::freeze::FreezeArgs),
    /// Load a frozen journal CSV into the SQLite core tables
    LoadJournal(db::load::Args),
    /// Check every balance assertion against the postings; fails on any
    /// mismatch
    Check(Database),
    /// Export the SQLite ledger as an hledger journal (audit with `hledger
    /// check`)
    ExportHledger(ExportHledger),
    /// Import Cathay bank downloads into SQLite (scratch databases until the
    /// Cutover)
    ImportCathayBank(import::cathay_bank::Args),
    /// Fetch daily exchange rates so the ledger's currencies can be compared
    Rates(ledger::rates::Args),
}

#[derive(Parser, Debug)]
struct Database {
    #[arg(long, default_value = "sqlite:ledger-app.db")]
    database_url: String,
}

#[derive(Parser, Debug)]
struct ExportHledger {
    #[command(flatten)]
    database: Database,

    /// Where to write the journal (financial data; keep it out of git)
    #[arg(long)]
    output: PathBuf,
}

#[derive(Parser, Debug)]
struct Record {
    #[arg(short, long, default_value = "manual_transactions.csv")]
    output_file: String,
}

impl Cli {
    pub async fn run(&self) -> Result<()> {
        match &self.command {
            Some(Command::Record(args)) => {
                record::run_interactive_record_session(&args.output_file).await
            }
            Some(Command::Ledger(args)) => {
                print!("{}", ledger::build(args)?);
                Ok(())
            }
            Some(Command::Freeze(args)) => {
                let report = ledger::freeze::run(args)?;
                print!("{report}");
                if report.ok() {
                    Ok(())
                } else {
                    anyhow::bail!("reconciliation failed; the journal was left as it was")
                }
            }
            Some(Command::LoadJournal(args)) => {
                print!("{}", db::load::run(args).await?);
                Ok(())
            }
            Some(Command::Check(args)) => {
                let pool = db::open_db(&args.database_url).await?;
                let report = db::check::check(&mut *pool.acquire().await?).await?;
                print!("{report}");
                match (report.ok(), report.figures()) {
                    (true, _) => Ok(()),
                    (false, 0) => {
                        anyhow::bail!("no balance figures: nothing vouches for this ledger")
                    }
                    (false, _) => {
                        anyhow::bail!("{} balance figures failed", report.mismatches.len())
                    }
                }
            }
            Some(Command::ExportHledger(args)) => {
                let pool = db::open_db(&args.database.database_url).await?;
                let journal = db::hledger::export(&mut *pool.acquire().await?).await?;
                std::fs::write(&args.output, journal)?;
                println!("wrote {}", args.output.display());
                Ok(())
            }
            Some(Command::ImportCathayBank(args)) => {
                print!("{}", import::cathay_bank::run(args).await?);
                Ok(())
            }
            Some(Command::Rates(args)) => {
                let (prices, _) = tracker_stores().await?;
                let written = ledger::rates::fetch(args, &prices).await?;
                println!("wrote {written} price directives");
                Ok(())
            }
            Some(Command::Init(args)) => calculate::init(args, tracker_stores()).await,
            None => calculate::run(&self.calculate_args, tracker_stores()).await,
        }
    }
}

/// The stock tracker's database, which caches quotes and splits.
async fn tracker_stores() -> Result<(PriceService<StockPriceStore>, StockSplitStore)> {
    let pool = db::init_db("sqlite:sqlite.db").await?;
    Ok((PriceService::new(StockPriceStore::new(pool.clone())), StockSplitStore::new(pool)))
}
