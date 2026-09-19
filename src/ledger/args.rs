//! Command-line arguments for building the ledger.

use std::path::PathBuf;

/// What to import, and where the ledger lives.
#[derive(clap::Parser, Debug)]
pub struct Args {
    /// Cathay bank statement exports, joined per account and currency
    #[arg(long, num_args = 1..)]
    pub cathay_statements: Vec<PathBuf>,

    /// 天天記帳 收支 export (income/expense)
    #[arg(long, conflicts_with = "transactions")]
    pub daily_income_expense: Option<PathBuf>,

    /// 天天記帳 轉帳 export (transfers)
    #[arg(long, conflicts_with = "transactions")]
    pub daily_transfers: Option<PathBuf>,

    /// corrected/transactions.csv, in place of the two 天天記帳 exports
    #[arg(long)]
    pub transactions: Option<PathBuf>,

    /// Also emit 天天記帳 history from before the statements begin
    #[arg(long = "daily-backfill")]
    pub backfill: bool,

    /// Directory holding the Beancount ledger
    #[arg(long, default_value = "ledger")]
    pub ledger_dir: PathBuf,
}
