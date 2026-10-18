//! The `rates` command: one fetch run from the command line.

use std::fmt;

use anyhow::Result;
use chrono::Local;
use ledger_types::currency::Currency;
use prices::{
    rates::{self, Coverage, Fetched},
    YFinanceSource,
};

use super::{Market, Rates, BASE, DEFAULT_URL, MAX_REQUESTS};

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// The ledger whose currencies need rates
    #[arg(long, default_value = "sqlite:ledger-app.db")]
    pub database_url: String,

    /// The market cache the rates go into
    #[arg(long, default_value = DEFAULT_URL)]
    pub market_url: String,

    /// Currency to quote every rate in
    #[arg(long, value_enum, default_value_t = BASE, hide_possible_values = true)]
    pub base: Currency,

    /// Most requests to make; the gaps left wait for the next run
    #[arg(long, default_value_t = MAX_REQUESTS)]
    pub max_requests: usize,
}

/// What `rates` did, and what still has no rate.
#[derive(Debug)]
pub struct Report {
    pub fetched: Fetched,
    pub coverage: Vec<Coverage>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.fetched)?;
        write!(f, "{}", Unrated(&self.coverage))
    }
}

/// The currencies with no rate at all, or a line saying there are none.
pub struct Unrated<'a>(pub &'a [Coverage]);

impl fmt::Display for Unrated<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unrated: Vec<String> =
            self.0.iter().filter(|c| c.unrated()).map(|c| c.currency.to_string()).collect();
        match unrated.is_empty() {
            true => writeln!(f, "every currency has a rate"),
            false => writeln!(
                f,
                "no rate for {}: their amounts stay unconverted until `rates` fetches them",
                unrated.join(", ")
            ),
        }
    }
}

/// Fetches every rate the ledger is missing into the cache.
pub async fn run(args: &Args) -> Result<Report> {
    let rates = Rates {
        ledger: crate::open_db(&args.database_url).await?,
        market: Market::open(&args.market_url).await?,
    };
    let today = Local::now().date_naive();
    let fetched =
        rates::fetch(&rates, &YFinanceSource::new(), args.base, today, args.max_requests).await?;
    let coverage = rates::status(&rates, args.base, today).await?;
    Ok(Report { fetched, coverage })
}
