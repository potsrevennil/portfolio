//! Command-line arguments for building the ledger.

/// What to import, and where the ledger lives.
#[derive(clap::Parser, Debug)]
pub struct Args {
    /// Cathay bank statement exports (活存 / 投資)
    #[arg(long, num_args = 1..)]
    pub cathay_statements: Vec<String>,

    /// 天天記帳 收支 export (income/expense)
    #[arg(long)]
    pub daily_income_expense: Option<String>,

    /// 天天記帳 轉帳 export (transfers)
    #[arg(long)]
    pub daily_transfers: Option<String>,

    /// Also emit 天天記帳 history from before the statements begin
    #[arg(long = "daily-backfill")]
    pub backfill: bool,

    /// Directory holding the Beancount ledger
    #[arg(long, default_value = "ledger")]
    pub ledger_dir: String,
}
