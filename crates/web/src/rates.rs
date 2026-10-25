//! The server's rate fetch: one background worker that fills the market
//! cache's gaps at start, daily, and as soon as the ledger holds a currency,
//! or an earlier day of one, it did not before. Never on a page's path: a
//! page reads only what the cache already has.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Local, NaiveTime};
use db::market::{Rates, BASE};
use prices::{
    rates::{Busy, RateStore},
    schedule::{self, Handle, Schedule},
    yahoo::YahooQuotes,
};

/// After the US close (16:00 New York is 20:00 or 21:00 UTC), when the day's
/// quotes are final. `LEDGER_RATES_AT` overrides it.
const DAILY_AT: &str = "22:00";

/// The first retry after a failed run; it doubles with each further failure.
const RETRY: Duration = Duration::from_secs(5 * 60);

/// How often the ledger is checked for a currency it newly holds. Imports run
/// as separate processes, so the worker cannot be told directly.
const WATCH_EVERY: Duration = Duration::from_secs(60);

pub struct Config {
    schedule: Schedule,
}

impl Config {
    /// `LEDGER_RATES=off` turns the fetch off; `LEDGER_RATES_AT=HH:MM` sets
    /// the daily run, in UTC.
    pub fn from_env() -> Result<Option<Self>> {
        match std::env::var("LEDGER_RATES").is_ok_and(|v| v == "off") {
            true => Ok(None),
            false => {
                let at = std::env::var("LEDGER_RATES_AT").unwrap_or_else(|_| DAILY_AT.into());
                let at = NaiveTime::parse_from_str(&at, "%H:%M")
                    .with_context(|| format!("LEDGER_RATES_AT={at:?} is not HH:MM"))?;
                Ok(Some(Self { schedule: Schedule { daily_at: Some(at), retry: RETRY } }))
            }
        }
    }
}

/// Starts the worker and the watch that wakes it.
pub fn start(rates: Rates, config: Config) {
    let handle = schedule::spawn(config.schedule, {
        let rates = rates.clone();
        move || run(rates.clone())
    });
    tokio::spawn(watch(rates, handle));
}

async fn run(rates: Rates) -> Result<()> {
    let today = Local::now().date_naive();
    let source = YahooQuotes::new()?;
    match prices::rates::fetch(&rates, &source, BASE, today).await {
        Ok(fetched) => {
            if fetched.requests > 0 {
                print!("rates: {fetched}");
            }
            Ok(())
        }
        // A `rates` run from the command line holds the lease and fills the
        // same gaps.
        Err(e) if e.is::<Busy>() => Ok(()),
        Err(e) => {
            eprintln!("rates: {e:#}");
            Err(e)
        }
    }
}

/// Requests a run whenever what the ledger needs changes, so an import that
/// brings a new currency is priced without waiting for the daily run.
async fn watch(rates: Rates, handle: Handle) {
    let mut seen = None;
    let mut tick = tokio::time::interval(WATCH_EVERY);
    loop {
        tick.tick().await;
        match rates.needs(BASE).await {
            Ok(needs) => {
                if seen.as_ref().is_some_and(|seen| *seen != needs) {
                    handle.request();
                }
                seen = Some(needs);
            }
            Err(e) => eprintln!("rates: reading the ledger's currencies: {e:#}"),
        }
    }
}
