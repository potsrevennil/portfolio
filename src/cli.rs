//! Command-line surface: argument structs and dispatch, nothing else.
//!
//! Each command's work lives in the module that owns it, so this file says what
//! the commands are and not how any of them is carried out.

use anyhow::Result;
use clap::Parser;

use crate::{calculate, db, ledger, prices::PriceService, record};

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
    LoadJournal(ledger::load::Args),
    /// Fetch daily exchange rates so the ledger's currencies can be compared
    Rates(ledger::rates::Args),
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
                    anyhow::bail!("reconciliation failed; journal not written")
                }
            }
            Some(Command::LoadJournal(args)) => {
                print!("{}", ledger::load::run(args).await?);
                Ok(())
            }
            Some(Command::Rates(args)) => {
                let pool = db::init_db("sqlite:sqlite.db").await?;
                let written = ledger::rates::fetch(args, &PriceService::new(pool)).await?;
                println!("wrote {written} price directives");
                Ok(())
            }
            Some(Command::Init(args)) => calculate::init(args).await,
            None => calculate::run(&self.calculate_args).await,
        }
    }
}
