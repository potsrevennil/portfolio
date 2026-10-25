//! Asking the source for the gaps, through the store the database provides.

use std::{collections::HashMap, fmt, future::Future};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use ledger_types::Currency;

use super::{
    coverage::{coverage, Coverage, Gap, Need},
    lease::{Busy, Lease},
    route::{fetch_plan, rated, tickers},
};
use crate::{source::StockPrice, span::Span};

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

    /// The days already asked of the source, per currency priced in `base`.
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

    /// Gives `holder` the fetch lease until `until`, unless another holder's
    /// lease is live at `now`.
    fn lease(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> impl Future<Output = Result<bool>> + Send;

    fn release(&self, holder: &str) -> impl Future<Output = Result<()>> + Send;

    /// A run's outcome: `error` is `None` when it succeeded.
    fn record_run(
        &self,
        at: DateTime<Utc>,
        error: Option<&str>,
    ) -> impl Future<Output = Result<()>> + Send;
}

/// Where quotes come from.
pub trait QuoteSource {
    /// Daily closes within `span` for each of `tickers`, in one request; a
    /// ticker missing from the answer has none. An error means the source
    /// could not be asked (offline, rate limited), never that it has nothing.
    fn quotes(
        &self,
        tickers: &[String],
        span: Span,
    ) -> impl Future<Output = Result<HashMap<String, Vec<StockPrice>>>> + Send;
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
}

impl fmt::Display for Fetched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} requests, {} quotes saved", self.requests, self.quotes)?;
        for gap in &self.asked {
            writeln!(f, "  asked: {gap}")?;
        }
        Ok(())
    }
}

/// Fetches every gap and records the run's outcome. Fails with [`Busy`]
/// while another process fetches. Reporting never calls this: it reads only
/// what is stored.
pub async fn fetch<S: RateStore>(
    store: &S,
    source: &impl QuoteSource,
    base: Currency,
    today: NaiveDate,
) -> Result<Fetched> {
    match Lease::take(store).await? {
        None => Err(Busy.into()),
        Some(lease) => {
            let started = Utc::now();
            let result = fill(store, source, base, today).await;
            let error = result.as_ref().err().map(|e| format!("{e:#}"));
            let recorded = store.record_run(started, error.as_deref()).await;
            let released = lease.release().await;
            // A failed run's own error matters more than failing to record it.
            match (result, recorded.and(released)) {
                (Err(e), _) => Err(e),
                (Ok(_), Err(e)) => Err(e.context("recording the run")),
                (Ok(fetched), Ok(())) => Ok(fetched),
            }
        }
    }
}

/// A gap on its way through the routes that could price it.
struct Pending {
    gap: Gap,
    plan: Vec<Vec<String>>,
    step: usize,
}

/// Each round asks, in one request, for the next step of every gap still
/// unpriced: the pairs, then the inverses, then the legs through USD. So a
/// run makes at most one request per step, however many currencies it fills.
async fn fill(
    store: &impl RateStore,
    source: &impl QuoteSource,
    base: Currency,
    today: NaiveDate,
) -> Result<Fetched> {
    let mut held = Held::read(store, base).await?;
    let mut gaps: Vec<Gap> = held.coverage(base, today).iter().flat_map(Coverage::gaps).collect();
    gaps.sort_by_key(|g| (g.span.from, g.currency));
    let mut pending: Vec<Pending> = gaps
        .into_iter()
        .map(|gap| Pending { plan: fetch_plan(gap.currency, base, &held.quotes), gap, step: 0 })
        .collect();

    let mut fetched = Fetched::default();
    while !pending.is_empty() {
        let mut tickers: Vec<String> =
            pending.iter().flat_map(|p| p.plan[p.step].clone()).collect();
        tickers.sort_unstable();
        tickers.dedup();
        let span = Span::new(
            pending.iter().map(|p| p.gap.span.from).min().expect("a pending gap"),
            pending.iter().map(|p| p.gap.span.through).max().expect("a pending gap"),
        );
        fetched.requests += 1;
        let got = source
            .quotes(&tickers, span)
            .await
            .with_context(|| format!("fetching {} for {span}", tickers.join(", ")))?;
        for ticker in tickers {
            let quotes = got.get(&ticker).map_or(&[][..], Vec::as_slice);
            store.save_quotes(&ticker, quotes).await?;
            fetched.quotes += quotes.len();
            held.quotes.entry(ticker).or_default().extend(quotes.iter().map(|q| q.date));
        }
        // A gap is done once priced, or once every route has been asked.
        let mut next = Vec::new();
        for mut p in std::mem::take(&mut pending) {
            p.step += 1;
            let priced = p.gap.span.is_covered_by(&rated(p.gap.currency, base, &held.quotes));
            match priced || p.step == p.plan.len() {
                true => {
                    store.record_asked(p.gap.currency, base, p.gap.span).await?;
                    fetched.asked.push(p.gap);
                }
                false => next.push(p),
            }
        }
        pending = next;
    }
    fetched.asked.sort_by_key(|g| (g.span.from, g.currency));
    Ok(fetched)
}
