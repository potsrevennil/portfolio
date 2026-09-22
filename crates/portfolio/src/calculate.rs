//! The portfolio report: load transactions, value them, print the totals.
//!
//! Kept out of `cli`, which holds the command surface only. The two entry
//! points differ in where the transactions come from — `init` rebuilds the
//! transaction file from broker reports first — and share everything after.

use anyhow::Result;
use chrono::{Duration, Months, NaiveDate, Utc};
use clap::{builder::PossibleValue, Parser};
use prices::PriceService;

use crate::{
    event::{self, loader::DataSource},
    portfolio::{
        consolidated::{ConsolidatedPortfolio, ConsolidatedPortfolioDisplay},
        Currency, Portfolio,
    },
    securities::Securities,
    split::store::SplitStore,
    Order, SortBy,
};

#[derive(Debug, Clone)]
pub enum TimeSelector {
    Year(u32),
    Month(u32),
    Week(u32),
    Day(u32),
    Date(NaiveDate),
}

impl TimeSelector {
    /// The start of the window this selector names, counting back from `end`.
    /// Saturates at `NaiveDate::MIN` rather than failing, so an absurdly long
    /// window reports everything instead of erroring.
    fn start_from(&self, end: NaiveDate) -> NaiveDate {
        let back = |date: Option<NaiveDate>| date.unwrap_or(NaiveDate::MIN);
        match self {
            TimeSelector::Year(y) => back(end.checked_sub_months(Months::new(y * 12))),
            TimeSelector::Month(m) => back(end.checked_sub_months(Months::new(*m))),
            TimeSelector::Week(w) => back(end.checked_sub_signed(Duration::weeks((*w).into()))),
            TimeSelector::Day(d) => back(end.checked_sub_signed(Duration::days((*d).into()))),
            TimeSelector::Date(d) => *d,
        }
    }
}

fn parse_time_selector(s: &str) -> Result<TimeSelector, String> {
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y%m%d") {
        return Ok(TimeSelector::Date(date));
    }
    let re = regex::Regex::new(r"(?i)^(\d+)([ymwd])$").map_err(|e| e.to_string())?;
    let caps = re.captures(s).ok_or_else(|| {
        "Invalid time selector format. Use e.g., '1y', '6M', '2w', '7d' or 'YYYYMMDD'".to_string()
    })?;
    let value: u32 = caps[1].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    match caps[2].to_lowercase().as_str() {
        "y" => Ok(TimeSelector::Year(value)),
        "m" => Ok(TimeSelector::Month(value)),
        "w" => Ok(TimeSelector::Week(value)),
        "d" => Ok(TimeSelector::Day(value)),
        _ => Err("Invalid duration unit".to_string()),
    }
}

#[derive(Parser, Debug, Clone)]
pub struct Init {
    /// One or more file paths for IB reports
    #[arg(long, num_args = 1..)]
    ib_files: Vec<String>,

    /// One or more file paths for Cathay reports
    #[arg(long, num_args = 1..)]
    cathay_files: Vec<String>,

    #[arg(long, num_args = 1..)]
    transactions_files: Vec<String>,

    #[arg(short, long, default_value = "transactions.csv")]
    output_file: String,

    #[command(flatten)]
    args: SharedArgs,
}

#[derive(Parser, Debug, Clone)]
pub struct Args {
    #[arg(default_value = "transactions.csv", num_args = 1..)]
    transactions_files: Vec<String>,

    #[command(flatten)]
    args: SharedArgs,
}

#[derive(Parser, Debug, Clone)]
struct SharedArgs {
    #[arg(short, long, value_enum, default_value_t = SortBy::Percentage)]
    sort_by: SortBy,

    #[arg(
        long,
        default_value = "-",
        value_parser = [
            PossibleValue::new("+").help("Ascending"),
            PossibleValue::new("-").help("Descending"),
        ]
    )]
    order: String,

    #[arg(short, long)]
    group: bool,

    #[arg(long, value_parser = parse_time_selector)]
    from: Option<TimeSelector>,

    #[arg(long, value_enum, default_value_t = Currency::USD, hide_possible_values = true)]
    reporting_currency: Currency,
}

/// Rebuild the transaction file from the broker reports, then report on it.
pub async fn init(init: &Init) -> Result<()> {
    let sources = vec![
        DataSource::Ib(init.ib_files.clone()),
        DataSource::Cathay(init.cathay_files.clone()),
        DataSource::Generic(init.transactions_files.clone()),
    ];
    report(sources, Some(init.output_file.clone()), &init.args).await
}

/// Report on the transaction file as it already stands.
pub async fn run(args: &Args) -> Result<()> {
    let sources = vec![DataSource::Generic(args.transactions_files.clone())];
    report(sources, None, &args.args).await
}

async fn report(
    sources: Vec<DataSource>,
    output_file: Option<String>,
    args: &SharedArgs,
) -> Result<()> {
    let securities = Securities::load(Securities::PATH)?;
    let pool = db::init_db("sqlite:sqlite.db").await?;
    let price_service = PriceService::new(pool.clone());
    let split_store = SplitStore::new(pool);

    let broker_data = event::load(sources, output_file, &split_store, &securities).await?;
    let mut portfolios = ConsolidatedPortfolio::from(
        broker_data.into_iter().map(|(b, (es, ss))| (b, Portfolio::new(b, es, ss))).collect(),
    );

    let end = Utc::now().date_naive();
    let start = args.from.as_ref().map_or(end, |from| from.start_from(end));

    let prices = portfolios.get_prices(args.reporting_currency, start, end, &price_service).await?;
    portfolios.generate_daily_statements(start, end, &prices);
    portfolios.calculate_totals(args.reporting_currency, &prices);

    let order = match args.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!("clap accepts only + or -"),
    };

    println!("{}", ConsolidatedPortfolioDisplay {
        consolidated_portfolio: &portfolios,
        sort_by: args.sort_by,
        order,
        prices: &prices,
    });
    Ok(())
}
