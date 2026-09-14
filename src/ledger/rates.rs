//! Daily exchange rates, so balances in different currencies can be compared.
//!
//! Without these the ledger holds nine currencies that cannot be added up, and
//! any total mixing them is meaningless. Beancount does not infer rates from
//! the `@@` annotations on transfers — the price map comes out empty — so they
//! have to be stated.
//!
//! Once present, Fava's own conversion selector works on every report, and the
//! overview page can offer a base currency.

use std::{collections::BTreeSet, fmt::Write as _, path::Path};

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate, Utc};

use crate::{currency::Currency, prices::PriceService};

#[derive(clap::Parser, Debug)]
pub struct Args {
    /// Currency to quote everything in
    #[arg(long, value_enum, default_value_t = Currency::TWD)]
    pub base: Currency,

    /// Directory holding the Beancount ledger
    #[arg(long, default_value = "ledger")]
    pub ledger_dir: String,
}

/// Currencies the generated ledger actually holds, and the date it starts.
///
/// Read back from the output rather than taken as arguments, so adding an
/// account in a new currency needs no second change here.
fn scan(dir: &Path) -> Result<(BTreeSet<Currency>, NaiveDate)> {
    let mut currencies = BTreeSet::new();
    let mut earliest: Option<NaiveDate> = None;

    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "beancount") {
            continue;
        }
        for line in std::fs::read_to_string(&path)?.lines() {
            let line = line.trim();
            if let Some(date) = line.get(..10).and_then(|d| {
                NaiveDate::parse_from_str(d, "%Y-%m-%d").ok().filter(|d| d.year() > 2000)
            }) {
                earliest = Some(earliest.map_or(date, |e: NaiveDate| e.min(date)));
            }
            // A posting ends with its amount and currency; keep the last token
            // only when it parses as a currency we know.
            if let Some(currency) =
                line.split_whitespace().last().and_then(|t| t.parse::<Currency>().ok())
            {
                currencies.insert(currency);
            }
        }
    }

    let start = earliest.context("no dated entries found; build the ledger first")?;
    Ok((currencies, start))
}

pub async fn fetch(args: &Args, service: &PriceService) -> Result<usize> {
    let dir = Path::new(&args.ledger_dir).join("generated");
    let (mut currencies, start) = scan(&dir)?;
    currencies.remove(&args.base);

    if currencies.is_empty() {
        println!("nothing to convert: the ledger only holds {}", args.base);
        return Ok(0);
    }

    // Yahoo quotes a pair as <from><to>=X, giving <to> per unit of <from>.
    // Thinly traded pairs are only published the other way round — there is no
    // VNDTWD=X, but TWDVND=X exists — so ask for both and invert the reverse
    // one. Without this VND and CNY stay unconverted, and Fava then plots them
    // as their own series stacked into the same bar as TWD.
    let forward: Vec<String> = currencies.iter().map(|c| format!("{}{}=X", c, args.base)).collect();
    let reverse: Vec<String> = currencies.iter().map(|c| format!("{}{}=X", args.base, c)).collect();
    // Last resort: Yahoo publishes nothing for TWD/CNY or TWD/VND in either
    // direction, but quotes both against USD, so the rate can be crossed.
    let via_usd: Vec<String> = currencies.iter().map(|c| format!("USD{c}=X")).collect();
    let base_usd = format!("USD{}=X", args.base);
    let tickers: Vec<String> = forward
        .iter()
        .chain(&reverse)
        .chain(&via_usd)
        .cloned()
        .chain(std::iter::once(base_usd.clone()))
        .collect();
    let refs: Vec<&str> = tickers.iter().map(String::as_str).collect();
    let end = Utc::now().date_naive();

    println!("fetching {} .. {} for {:?}", start, end, currencies);
    let fetched = service.get_prices(&refs, start, end).await?;

    let mut out = String::new();
    writeln!(out, ";; GENERATED — do not edit by hand.")?;
    writeln!(out, ";; Daily rates quoted in {}, so multi-currency balances can be", args.base)?;
    writeln!(out, ";; compared. Fava's conversion selector uses these.\n")?;

    // USD per base, indexed by date, for the crossing fallback.
    let base_per_usd: std::collections::HashMap<NaiveDate, f64> = fetched
        .get(&base_usd)
        .map(|points| points.iter().map(|p| (p.date, p.close_price)).collect())
        .unwrap_or_default();

    let mut rows: Vec<(NaiveDate, Currency, f64)> = Vec::new();
    for (((currency, direct), inverse), usd) in
        currencies.iter().zip(&forward).zip(&reverse).zip(&via_usd)
    {
        let series = |t: &String| fetched.get(t).map(Vec::as_slice).unwrap_or_default();

        // Take whichever source has the most history, not the first that
        // returns anything. Yahoo answers CNYTWD=X with a single spot quote
        // while serving a full series for TWDCNY=X — preferring the direct pair
        // because it was non-empty left four years unconverted.
        // Ranked so that, at equal coverage, the quote needing least arithmetic
        // wins: the direct pair, then the inverted one, then the USD cross,
        // which compounds two conversions and their rounding.
        let candidates: [(u8, Vec<(NaiveDate, f64)>); 3] = [
            (2, series(direct).iter().map(|p| (p.date, p.close_price)).collect()),
            (
                1,
                series(inverse)
                    .iter()
                    .filter(|p| p.close_price != 0.0)
                    .map(|p| (p.date, 1.0 / p.close_price))
                    .collect(),
            ),
            (
                0,
                // p is <currency> per USD; base_per_usd is <base> per USD.
                series(usd)
                    .iter()
                    .filter(|p| p.close_price != 0.0)
                    .filter_map(|p| base_per_usd.get(&p.date).map(|b| (p.date, b / p.close_price)))
                    .collect(),
            ),
        ];
        let Some((_, points)) = candidates
            .into_iter()
            .filter(|(_, p)| !p.is_empty())
            .max_by_key(|(rank, p)| (p.len(), *rank))
        else {
            println!("  no rates for {currency} — balances in it stay unconverted");
            continue;
        };
        // Some pairs come back as a single spot quote with no history. Beancount
        // cannot convert anything dated before the earliest price, so a lone
        // quote leaves years of spending unconverted, and Fava then plots that
        // currency as its own series stacked beside the converted one.
        // Extending the rate back to the start is an approximation, but a
        // visibly flat line is honest about that, whereas an unconvertible
        // series silently invites comparing different units.
        if let [(date, rate)] = points[..] {
            if date > start {
                rows.push((start, *currency, rate));
                println!(
                    "  {currency}: only a spot quote, held flat from {start} — historical amounts \
                     are approximate"
                );
            }
        }
        rows.extend(points.into_iter().map(|(date, rate)| (date, *currency, rate)));
    }
    rows.sort_by_key(|(date, currency, _)| (*date, currency.to_string()));
    for (date, currency, rate) in &rows {
        writeln!(out, "{} price {} {:.6} {}", date, currency, rate, args.base)?;
    }

    std::fs::write(dir.join("rates.beancount"), out)?;
    Ok(rows.len())
}
