//! Asking the source for the gaps, through the store the database provides.

use std::{collections::HashMap, fmt, future::Future};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use ledger_types::Currency;

use super::{
    coverage::{coverage, Coverage, Gap, Need},
    route::{fetch_plan, rated, tickers},
};
use crate::{
    source::StockPrice,
    span::{uncovered, Span},
};

/// The database side of the fetch; `db` implements it, so this crate stays
/// free of SQL.
pub trait RateStore {
    /// Every currency the ledger holds other than `base`, with its first day.
    fn needs(&self, base: Currency) -> impl Future<Output = Result<Vec<Need>>> + Send;

    /// The dates each ticker has a quote for.
    fn quote_dates(
        &self,
        tickers: &[String],
    ) -> impl Future<Output = Result<HashMap<String, Vec<NaiveDate>>>> + Send;

    /// What was already asked of the source, per currency priced in `base`.
    fn asked(
        &self,
        base: Currency,
    ) -> impl Future<Output = Result<HashMap<Currency, Vec<Span>>>> + Send;

    fn save_quotes(
        &self,
        ticker: &str,
        quotes: &[StockPrice],
    ) -> impl Future<Output = Result<()>> + Send;

    fn record_asked(
        &self,
        currency: Currency,
        base: Currency,
        span: Span,
    ) -> impl Future<Output = Result<()>> + Send;

    /// A run's outcome: `error` is `None` when it succeeded.
    fn record_run(
        &self,
        at: DateTime<Utc>,
        error: Option<&str>,
    ) -> impl Future<Output = Result<()>> + Send;
}

/// Where quotes come from.
pub trait QuoteSource {
    /// Daily closes for `ticker` within `span`, empty when the source has
    /// none. An error means the source could not be asked (offline, rate
    /// limited), never that it has nothing.
    fn quotes(
        &self,
        ticker: &str,
        span: Span,
    ) -> impl Future<Output = Result<Vec<StockPrice>>> + Send;
}

/// What the store holds, read once for a run.
struct Held {
    needs: Vec<Need>,
    quotes: HashMap<String, Vec<NaiveDate>>,
    asked: HashMap<Currency, Vec<Span>>,
}

impl Held {
    async fn read(store: &impl RateStore, base: Currency) -> Result<Self> {
        let needs = store.needs(base).await?;
        let mut tickers: Vec<String> =
            needs.iter().flat_map(|n| tickers(n.currency, base)).collect();
        tickers.sort_unstable();
        tickers.dedup();
        let quotes = store.quote_dates(&tickers).await?;
        let asked = store.asked(base).await?;
        Ok(Self { needs, quotes, asked })
    }

    fn coverage(&self, base: Currency, today: NaiveDate) -> Vec<Coverage> {
        coverage(&self.needs, base, today, &self.quotes, &self.asked)
    }
}

/// Coverage of every currency the ledger holds, from the store.
pub async fn status(
    store: &impl RateStore,
    base: Currency,
    today: NaiveDate,
) -> Result<Vec<Coverage>> {
    Ok(Held::read(store, base).await?.coverage(base, today))
}

/// What one fetch run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    pub requests: usize,
    pub quotes: usize,
    /// Gaps asked about, whether or not the source had quotes for them.
    pub asked: Vec<Gap>,
    /// Gaps the request cap left for the next run.
    pub left: Vec<Gap>,
}

impl fmt::Display for Fetched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} requests, {} quotes saved", self.requests, self.quotes)?;
        for gap in &self.asked {
            writeln!(f, "  asked: {gap}")?;
        }
        for gap in &self.left {
            writeln!(f, "  left for the next run: {gap}")?;
        }
        Ok(())
    }
}

/// Fetches every gap, oldest first, making at most `max_requests` requests,
/// and records the run's outcome. Reporting never calls this: it reads only
/// what is stored.
pub async fn fetch(
    store: &impl RateStore,
    source: &impl QuoteSource,
    base: Currency,
    today: NaiveDate,
    max_requests: usize,
) -> Result<Fetched> {
    let started = Utc::now();
    let result = fill(store, source, base, today, max_requests).await;
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    let recorded = store.record_run(started, error.as_deref()).await;
    // A failed run's own error matters more than failing to record it.
    match (result, recorded) {
        (Err(e), _) => Err(e),
        (Ok(_), Err(e)) => Err(e.context("recording the run")),
        (Ok(fetched), Ok(())) => Ok(fetched),
    }
}

async fn fill(
    store: &impl RateStore,
    source: &impl QuoteSource,
    base: Currency,
    today: NaiveDate,
    max_requests: usize,
) -> Result<Fetched> {
    let mut held = Held::read(store, base).await?;
    let mut gaps: Vec<Gap> = held.coverage(base, today).iter().flat_map(Coverage::gaps).collect();
    gaps.sort_by_key(|g| (g.span.from, g.currency));

    let mut fetched = Fetched::default();
    let mut asked: HashMap<String, Vec<Span>> = HashMap::new();
    for gap in gaps {
        let mut capped = false;
        'plan: for step in fetch_plan(gap.currency, base, &held.quotes) {
            for ticker in step {
                // A leg this run already asked for across the gap (USD→base,
                // usually) needs no second request.
                if uncovered(gap.span, asked.get(&ticker).map_or(&[], Vec::as_slice)).is_empty() {
                    continue;
                }
                if fetched.requests >= max_requests {
                    capped = true;
                    break 'plan;
                }
                fetched.requests += 1;
                let got = source
                    .quotes(&ticker, gap.span)
                    .await
                    .with_context(|| format!("fetching {ticker} for {}", gap.span))?;
                store.save_quotes(&ticker, &got).await?;
                fetched.quotes += got.len();
                asked.entry(ticker.clone()).or_default().push(gap.span);
                held.quotes.entry(ticker).or_default().extend(got.iter().map(|q| q.date));
            }
            if gap.span.is_covered_by(&rated(gap.currency, base, &held.quotes)) {
                break;
            }
        }
        match capped {
            true => fetched.left.push(gap),
            false => {
                store.record_asked(gap.currency, base, gap.span).await?;
                fetched.asked.push(gap);
            }
        }
    }
    Ok(fetched)
}
