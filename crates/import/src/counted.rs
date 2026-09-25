//! A balance the user counted, for an account no institution reports on:
//! recorded as a `counted` assertion, which the gate then holds the ledger to
//! as it does a statement's closing.

use std::{fmt, path::PathBuf};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use db::{
    assertions::{self, AssertionSource, BalanceAssertion},
    check,
    import::ensure_account,
    import_batch, SqliteConnection,
};
use ledger::{accounts::Chart, labels::Labels};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// A ledger path (Assets:Cash:TWD) or the 天天記帳 label mapping.toml maps.
    #[arg(long)]
    pub account: String,

    /// The day the count holds for, at its end.
    #[arg(long)]
    pub date: NaiveDate,

    #[arg(long)]
    pub amount: Decimal,

    /// Needed only when the account holds none or several.
    #[arg(long)]
    pub currency: Option<Currency>,

    /// No default: until the Cutover this runs against scratch databases only.
    #[arg(long)]
    pub database_url: String,

    /// Directory holding mapping.toml.
    #[arg(long, default_value = "ledger")]
    pub ledger_dir: PathBuf,
}

#[derive(Debug)]
pub struct Report {
    pub assertion: BalanceAssertion,
    pub check: check::CheckReport,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "recorded {}: {}", self.assertion, self.assertion.closing)?;
        write!(f, "{}", self.check)
    }
}

pub async fn run(args: &Args) -> Result<Report> {
    let chart = Chart::load(args.ledger_dir.join("mapping.toml"))?;
    let pool = db::init_db(&args.database_url).await?;
    let mut tx = pool.begin().await?;
    let account = match chart.account(&args.account) {
        Some(mapping) => mapping.account.to_string(),
        None => args.account.clone(),
    };
    let report = record(&mut tx, &chart, &account, args.date, args.amount, args.currency).await?;
    tx.commit().await?;
    Ok(report)
}

/// Records the count in the caller's transaction and gates it; commit only
/// on `Ok`. A count the postings disagree with fails the gate.
pub async fn record(
    db: &mut SqliteConnection,
    chart: &Chart,
    account: &str,
    date: NaiveDate,
    amount: Decimal,
    currency: Option<Currency>,
) -> Result<Report> {
    if !chart.is_counted(account) {
        bail!("{account} is not under a [counted] root in mapping.toml");
    }
    let currency = match (currency, import_batch::currencies(db, account).await?.as_slice()) {
        (Some(c), _) => c,
        (None, [only]) => *only,
        (None, []) => bail!("{account} holds nothing yet; name the --currency"),
        (None, many) => bail!("{account} holds {many:?}; name the --currency"),
    };
    let assertion = BalanceAssertion {
        source: AssertionSource::Counted,
        account: account.to_string(),
        currency,
        period_start: None,
        opening: None,
        period_end: date,
        closing: amount,
    };
    ensure_account(db, &Labels::from(chart), account).await?;
    assertions::insert(db, &assertion).await?;
    let check = check::gate_with_counts(db, chart)
        .await
        .context("the count disagrees with the ledger, so it was not recorded")?;
    Ok(Report { assertion, check })
}
