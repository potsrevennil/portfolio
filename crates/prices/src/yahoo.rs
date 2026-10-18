//! FX closes straight from Yahoo's chart endpoints, many tickers per request.
//! `spark` takes a list of symbols and a range back from today; `chart` takes
//! exact dates, for a span older than spark's longest range. Calling them
//! directly skips yfinance-rs, which looks up each symbol's company profile on
//! every history call: about a second each, for nothing FX pairs have.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::{rates::QuoteSource, source::StockPrice, span::Span};

const HOST: &str = "https://query1.finance.yahoo.com";

/// Symbols per spark request; Yahoo refuses longer lists.
const PER_REQUEST: usize = 20;

/// Spark's daily ranges, shortest first, with the days each reaches back.
/// Its `max` is monthly, so a longer span goes to `chart`.
const RANGES: [(&str, i64); 8] = [
    ("5d", 4),
    ("1mo", 28),
    ("3mo", 89),
    ("6mo", 180),
    ("1y", 364),
    ("2y", 729),
    ("5y", 1825),
    ("10y", 3650),
];

pub struct YahooQuotes {
    http: reqwest::Client,
}

impl YahooQuotes {
    pub fn new() -> Result<Self> {
        // Yahoo answers a request with no browser user agent with 429.
        let http = reqwest::Client::builder().user_agent("Mozilla/5.0").build()?;
        Ok(Self { http })
    }

    async fn spark(&self, tickers: &[String], range: &str) -> Result<Vec<SparkItem>> {
        let response = self
            .http
            .get(format!("{HOST}/v7/finance/spark"))
            .query(&[("symbols", tickers.join(",").as_str()), ("range", range), ("interval", "1d")])
            .send()
            .await?;
        match response.status() {
            // None of the symbols has data.
            StatusCode::NOT_FOUND => Ok(Vec::new()),
            status if status.is_success() => {
                Ok(response.json::<SparkBody>().await?.spark.result.unwrap_or_default())
            }
            status => bail!("spark answered {status}"),
        }
    }

    async fn chart(&self, ticker: &str, span: Span) -> Result<Vec<Chart>> {
        let start = span.from.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp();
        let end = span.through.and_hms_opt(23, 59, 59).expect("a time").and_utc().timestamp();
        let response = self
            .http
            .get(format!("{HOST}/v8/finance/chart/{ticker}"))
            .query(&[("period1", start), ("period2", end)])
            .query(&[("interval", "1d")])
            .send()
            .await?;
        match response.status() {
            StatusCode::NOT_FOUND => Ok(Vec::new()),
            status if status.is_success() => {
                Ok(response.json::<ChartBody>().await?.chart.result.unwrap_or_default())
            }
            status => bail!("chart answered {status}"),
        }
    }
}

impl QuoteSource for YahooQuotes {
    async fn quotes(
        &self,
        tickers: &[String],
        span: Span,
    ) -> Result<HashMap<String, Vec<StockPrice>>> {
        let mut quotes = HashMap::new();
        match range(Utc::now().date_naive(), span.from) {
            Some(range) => {
                for chunk in tickers.chunks(PER_REQUEST) {
                    let items = self
                        .spark(chunk, range)
                        .await
                        .with_context(|| format!("fetching {} for {span}", chunk.join(", ")))?;
                    for item in items {
                        let closes = item.response.iter().flat_map(|c| c.closes(span)).collect();
                        quotes.insert(item.symbol, closes);
                    }
                }
            }
            None => {
                for ticker in tickers {
                    let charts = self
                        .chart(ticker, span)
                        .await
                        .with_context(|| format!("fetching {ticker} for {span}"))?;
                    quotes.insert(
                        ticker.clone(),
                        charts.iter().flat_map(|c| c.closes(span)).collect(),
                    );
                }
            }
        }
        Ok(quotes)
    }
}

/// The shortest spark range reaching back to `from`; `None` past ten years.
fn range(today: NaiveDate, from: NaiveDate) -> Option<&'static str> {
    RANGES.iter().find(|(_, days)| today - Duration::days(*days) <= from).map(|(range, _)| *range)
}

#[derive(Deserialize)]
struct SparkBody {
    spark: SparkResults,
}

#[derive(Deserialize)]
struct SparkResults {
    result: Option<Vec<SparkItem>>,
}

#[derive(Deserialize)]
struct SparkItem {
    symbol: String,
    response: Vec<Chart>,
}

#[derive(Deserialize)]
struct ChartBody {
    chart: ChartResults,
}

#[derive(Deserialize)]
struct ChartResults {
    result: Option<Vec<Chart>>,
}

#[derive(Deserialize)]
struct Chart {
    #[serde(default)]
    timestamp: Vec<i64>,
    indicators: Indicators,
}

#[derive(Deserialize)]
struct Indicators {
    quote: Vec<Closes>,
}

#[derive(Deserialize)]
struct Closes {
    #[serde(default)]
    close: Vec<Option<f64>>,
}

impl Chart {
    /// The closes dated within `span`, by the bar's UTC date, as yfinance-rs
    /// dates them, so the cache keeps one convention.
    fn closes(&self, span: Span) -> Vec<StockPrice> {
        let closes = self.indicators.quote.first().map_or(&[][..], |q| q.close.as_slice());
        self.timestamp
            .iter()
            .zip(closes)
            .filter_map(|(&ts, close)| {
                let date = DateTime::from_timestamp(ts, 0)?.date_naive();
                close.map(|close_price| StockPrice { date, close_price })
            })
            .filter(|q| span.from <= q.date && q.date <= span.through)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{date, span};

    #[test]
    fn the_shortest_range_that_reaches_the_span() {
        let today = date("2026-10-25");
        assert_eq!(range(today, date("2026-10-23")), Some("5d"));
        assert_eq!(range(today, date("2026-09-01")), Some("3mo"));
        assert_eq!(range(today, date("2022-01-01")), Some("5y"));
        assert_eq!(range(today, date("2010-01-01")), None);
    }

    /// A spark answer, trimmed: the 23:00 UTC bars take their UTC date, a
    /// missing close is skipped, and only the span's days are kept.
    #[test]
    fn closes_come_from_the_spark_answer() {
        let body = r#"{"spark":{"result":[{"symbol":"USDTWD=X","response":[{
            "meta":{"currency":"TWD"},
            "timestamp":[1792364400,1792450800,1792537200,1792623600],
            "indicators":{"quote":[{"close":[32.1,null,32.3,32.4]}]}}]}],"error":null}}"#;
        let items = serde_json::from_str::<SparkBody>(body).unwrap().spark.result.unwrap();
        let got = items[0].response[0].closes(span("2026-10-18", "2026-10-20"));
        assert_eq!(got, vec![
            StockPrice { date: date("2026-10-18"), close_price: 32.1 },
            StockPrice { date: date("2026-10-20"), close_price: 32.3 },
        ]);
    }
}
