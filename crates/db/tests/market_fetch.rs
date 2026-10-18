//! The rate fetch (`db::market`'s store, `prices::rates::fetch`) against real
//! SQLite and a scripted source: which gaps it asks for, in what order, and
//! what it leaves for the next run. All quotes are invented.

use std::{collections::HashMap, sync::Mutex};

use anyhow::{bail, Result};
use chrono::{Datelike, NaiveDate, Utc};
use db::market::{LastRun, Rates, Unrated, RATES_JOB};
use ledger_types::Currency;
use prices::{
    rates::{self, Busy, QuoteSource, RateStore},
    span::Span,
    StockPrice,
};
use sqlx::SqlitePool;
use tempfile::TempDir;

#[path = "common/market.rs"]
mod market;

fn d(s: &str) -> NaiveDate { s.parse().unwrap() }

const TODAY: &str = "2024-06-30";

/// Answers from a fixed set of series, and remembers each request's tickers.
struct Source {
    series: HashMap<String, Vec<StockPrice>>,
    offline: bool,
    asked: Mutex<Vec<Vec<String>>>,
}

impl Source {
    fn new(tickers: &[&str]) -> Self {
        let weekdays = Span::new(d("2023-01-02"), d("2024-07-31"))
            .into_iter()
            .filter(|day| day.weekday().number_from_monday() <= 5)
            .map(|date| StockPrice { date, close_price: 2.0 })
            .collect::<Vec<_>>();
        Self {
            series: tickers.iter().map(|t| (t.to_string(), weekdays.clone())).collect(),
            offline: false,
            asked: Mutex::default(),
        }
    }

    fn asked(&self) -> Vec<Vec<String>> { std::mem::take(&mut self.asked.lock().unwrap()) }
}

impl QuoteSource for Source {
    async fn quotes(
        &self,
        tickers: &[String],
        span: Span,
    ) -> Result<HashMap<String, Vec<StockPrice>>> {
        self.asked.lock().unwrap().push(tickers.to_vec());
        match self.offline {
            true => bail!("offline"),
            false => Ok(tickers
                .iter()
                .filter_map(|t| self.series.get(t).map(|quotes| (t, quotes)))
                .map(|(t, quotes)| {
                    let within =
                        quotes.iter().filter(|q| span.from <= q.date && q.date <= span.through);
                    (t.clone(), within.cloned().collect())
                })
                .collect()),
        }
    }
}

/// A ledger holding USD from January, VND from February and JPY from March,
/// beside an empty market cache.
async fn fixture() -> (TempDir, Rates) {
    let dir = TempDir::new().unwrap();
    let ledger =
        db::init_db(&format!("sqlite:{}", dir.path().join("ledger.db").display())).await.unwrap();
    let cash = account(&ledger, "Assets:Cash").await;
    let opening = account(&ledger, "Equity:Opening-Balances").await;
    for (date, currency) in [("2024-01-02", "USD"), ("2024-02-01", "VND"), ("2024-03-01", "JPY")] {
        let id = sqlx::query("INSERT INTO transactions (date, source) VALUES (?, 'manual')")
            .bind(date)
            .execute(&ledger)
            .await
            .unwrap()
            .last_insert_rowid();
        for (account, amount) in [(cash, "1"), (opening, "-1")] {
            sqlx::query(
                "INSERT INTO postings (transaction_id, account_id, amount, currency) VALUES (?, \
                 ?, ?, ?)",
            )
            .bind(id)
            .bind(account)
            .bind(amount)
            .bind(currency)
            .execute(&ledger)
            .await
            .unwrap();
        }
    }
    let market = market::beside(dir.path()).await;
    (dir, Rates { ledger, market })
}

async fn account(pool: &SqlitePool, path: &str) -> i64 {
    let ty = if path.starts_with("Assets") { "asset" } else { "equity" };
    sqlx::query("INSERT INTO accounts (path, label, type) VALUES (?, ?, ?)")
        .bind(path)
        .bind(path)
        .bind(ty)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

async fn fetch(rates: &Rates, source: &Source) -> Result<rates::Fetched> {
    rates::fetch(rates, source, Currency::TWD, d(TODAY)).await
}

async fn last_run(rates: &Rates) -> LastRun {
    rates.market.last_run(RATES_JOB).await.unwrap().expect("a recorded run")
}

async fn status(rates: &Rates) -> Vec<rates::Coverage> {
    rates::status(rates, Currency::TWD, d(TODAY)).await.unwrap()
}

#[tokio::test]
async fn every_currency_is_asked_at_once_one_route_per_round() {
    let (_dir, rates) = fixture().await;
    // JPY is quoted only the other way round, VND only against USD.
    let source = Source::new(&["USDTWD=X", "TWDJPY=X", "USDVND=X"]);
    assert_eq!(
        Unrated(&status(&rates).await).to_string(),
        "no rate for JPY, USD, VND: their amounts stay unconverted until `rates` fetches them\n"
    );

    let fetched = fetch(&rates, &source).await.unwrap();
    assert_eq!(source.asked(), [
        // Every pair; only USD's has quotes.
        vec!["JPYTWD=X", "USDTWD=X", "VNDTWD=X"],
        // The inverses of the rest; JPY's has them.
        vec!["TWDJPY=X", "TWDVND=X"],
        // VND through USD.
        vec!["USDTWD=X", "USDVND=X"],
    ]);
    assert_eq!(fetched.asked.len(), 3);

    let after = status(&rates).await;
    assert!(after.iter().all(|c| c.gaps.is_empty() && !c.unrated()), "{after:?}");
    assert_eq!(Unrated(&after).to_string(), "every currency has a rate\n");

    // Nothing is missing, so the next run asks for nothing.
    assert_eq!(fetch(&rates, &source).await.unwrap().requests, 0);
    assert!(source.asked().is_empty());
}

/// Each day's run asks the route a currency is priced by for the days since
/// its newest quote, that one included in case it was fetched before the
/// close.
#[tokio::test]
async fn each_day_the_tail_since_the_newest_quote_is_asked() {
    let (_dir, rates) = fixture().await;
    let source = Source::new(&["USDTWD=X", "TWDJPY=X", "USDVND=X"]);
    fetch(&rates, &source).await.unwrap();
    source.asked();

    let tuesday = d("2024-07-02");
    let fetched = rates::fetch(&rates, &source, Currency::TWD, tuesday).await.unwrap();
    // One request: each currency's own route, VND's through USD.
    assert_eq!(source.asked(), [vec!["TWDJPY=X", "USDTWD=X", "USDVND=X"]]);
    assert_eq!(fetched.quotes, 9, "Friday again, Monday and Tuesday, for each");
    let after = rates::status(&rates, Currency::TWD, tuesday).await.unwrap();
    assert!(after.iter().all(|c| c.gaps.is_empty()), "{after:?}");
}

#[tokio::test]
async fn a_pair_with_no_quotes_is_asked_once() {
    let (_dir, rates) = fixture().await;
    let source = Source::new(&["USDTWD=X"]);
    fetch(&rates, &source).await.unwrap();
    source.asked();
    let after = status(&rates).await;
    let jpy = after.iter().find(|c| c.currency == Currency::JPY).unwrap();
    assert!(jpy.unrated() && jpy.gaps.is_empty(), "{jpy:?}");
    assert_eq!(fetch(&rates, &source).await.unwrap().requests, 0);
}

/// While another process holds the lease a run asks for nothing; once it is
/// released, or lapses because its holder crashed, the next run goes ahead.
#[tokio::test]
async fn one_fetch_at_a_time_across_processes() {
    let (_dir, rates) = fixture().await;
    let source = Source::new(&["USDTWD=X", "TWDJPY=X", "USDVND=X"]);
    let (now, minutes) = (Utc::now(), chrono::Duration::minutes);
    assert!(rates.lease("other", now, now + minutes(10)).await.unwrap());
    let err = fetch(&rates, &source).await.unwrap_err();
    assert!(err.is::<Busy>(), "{err:#}");
    assert!(source.asked().is_empty());

    rates.release("other").await.unwrap();
    assert!(rates.lease("crashed", now - minutes(20), now - minutes(10)).await.unwrap());
    assert_eq!(fetch(&rates, &source).await.unwrap().asked.len(), 3);
}

#[tokio::test]
async fn a_failed_run_is_recorded_and_its_gaps_asked_again() {
    let (_dir, rates) = fixture().await;
    let mut source = Source::new(&["USDTWD=X", "TWDJPY=X", "USDVND=X"]);
    source.offline = true;
    let err = fetch(&rates, &source).await.unwrap_err();
    assert!(format!("{err:#}").contains("offline"), "{err:#}");
    let run = last_run(&rates).await;
    assert!(run.last_success.is_none(), "{run:?}");
    assert!(run.last_error.as_deref().is_some_and(|e| e.contains("offline")), "{run:?}");
    assert_eq!(status(&rates).await.iter().filter(|c| !c.gaps.is_empty()).count(), 3);

    source.offline = false;
    fetch(&rates, &source).await.unwrap();
    let run = last_run(&rates).await;
    assert!(run.last_success == Some(run.last_attempt) && run.last_error.is_none(), "{run:?}");
}
