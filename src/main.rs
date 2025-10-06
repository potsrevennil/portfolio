use anyhow::Result;
use chrono::{Duration, Months, NaiveDate, Utc};
use clap::{builder::PossibleValue, Parser, Subcommand};
use portfolio::{
    cathay, db, fixed_splits, ib,
    portfolio::{Currency, PortfolioDisplay},
    prices::{PriceService, StockPriceStore},
    split::store::SplitStore,
    Order, Portfolio, SortBy, Splits, StockSplits,
};

#[derive(Debug, Clone)]
enum TimeSelector {
    Year(u32),
    Month(u32),
    Week(u32),
    Day(u32),
    Date(NaiveDate),
}

// Helper function to parse duration string (e.g., "1y", "6m", "2w", "7d") or a
// specific date (e.g., "20250901")
fn parse_time_selector(s: &str) -> Result<TimeSelector, String> {
    // Try to parse as a specific date (YYYYMMDD)
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y%m%d") {
        return Ok(TimeSelector::Date(date));
    }

    // If not a date, try to parse as a duration
    let re = regex::Regex::new(r"(?i)^(\d+)([ymwd])$").map_err(|e| e.to_string())?;
    let caps = re.captures(s).ok_or_else(|| {
        "Invalid time selector format. Use e.g., '1y', '6M', '2w', '7d' or 'YYYYMMDD'".to_string()
    })?;

    let value: u32 = caps[1].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let unit = caps[2].to_lowercase();

    match unit.as_str() {
        "y" => Ok(TimeSelector::Year(value)),
        "m" => Ok(TimeSelector::Month(value)),
        "w" => Ok(TimeSelector::Week(value)),
        "d" => Ok(TimeSelector::Day(value)),
        _ => Err("Invalid duration unit".to_string()),
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    calculate: Calculate,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Initialize and process broker data
    Init(Init),
}

#[derive(Parser, Debug)]
struct Init {
    #[command(subcommand)]
    broker: BrokerCommand,

    #[arg(short, long, default_value = "transactions.csv")]
    transactions_file: String,

    #[command(flatten)]
    args: SharedArgs,
}

#[derive(Subcommand, Debug)]
enum BrokerCommand {
    /// Process Interactive Brokers data from file(s)
    Ib {
        /// One or more file paths for IB reports
        #[arg(short, long, required = true, num_args = 1..)]
        files: Vec<String>,
    },
    /// Process Cathay data from file(s)
    Cathay {
        /// One or more file paths for Cathay reports
        #[arg(short, long, required = true, num_args = 1..)]
        files: Vec<String>,
    },
}

#[derive(Parser, Debug)]
struct Calculate {
    #[arg(short, long, default_value = "transactions.csv")]
    transactions_file: String,

    #[command(flatten)]
    args: SharedArgs,
}

#[derive(Parser, Debug)]
struct SharedArgs {
    #[arg(short, long, value_enum, default_value_t = SortBy::Percentage)]
    sort_by: SortBy,

    #[arg(
        short,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    let mut portfolio = Portfolio::new();

    // Initialize PriceService and StockPriceStore
    let pool = db::init_db("sqlite:sqlite.db").await?;
    let price_store = StockPriceStore::new(pool.clone());
    let price_service = PriceService::new(price_store.clone());

    // Initialize SplitService
    let split_store = SplitStore::new(pool.clone());
    let split_service = Splits::new(split_store.clone());

    let (shared_args, reporting_currency) = match cli.command {
        Some(Command::Init(init_args)) => {
            let current_reporting_currency = match init_args.broker {
                BrokerCommand::Ib { files } => {
                    for file in files {
                        ib::load_from_ib_csv(&mut portfolio, &file)?;
                    }
                    Currency::USD
                }
                BrokerCommand::Cathay { files } => {
                    for file in files {
                        cathay::load_from_cathay_csv(&mut portfolio, &file)?;
                    }

                    // Add fixed splits
                    for (date, splits) in fixed_splits() {
                        portfolio.events.entry(date).or_default().splits.extend(splits);
                    }
                    Currency::TWD
                }
            };

            // Extract splits from portfolio events and save them
            // let mut all_splits: portfolio::StockSplits = portfolio::StockSplits::new();
            let all_splits: StockSplits = portfolio
                .events
                .iter()
                .filter_map(|(&date, event)| {
                    if event.splits.is_empty() {
                        None
                    } else {
                        Some((date, event.splits.clone()))
                    }
                })
                .collect();
            split_service.save(&all_splits).await?;

            portfolio.to_csv_file(&init_args.transactions_file)?;
            (init_args.args, current_reporting_currency)
        }
        None => {
            portfolio.load_from_csv(&cli.calculate.transactions_file)?;
            // For calculate command, default to USD for now
            (cli.calculate.args, Currency::USD)
        }
    };

    let end = Utc::now().date_naive();

    let start = if let Some(ref from) = shared_args.from {
        match from {
            TimeSelector::Year(y) => {
                end.checked_sub_months(Months::new(*y * 12)).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Month(m) => {
                end.checked_sub_months(Months::new(*m)).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Week(w) => {
                end.checked_sub_signed(Duration::weeks((*w).into())).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Day(d) => {
                end.checked_sub_signed(Duration::days((*d).into())).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Date(d) => *d,
        }
    } else {
        end
    };

    let order = match shared_args.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(),
    };

    // Determine the start date for fetching prices
    let fetch_start = portfolio.events.first_key_value().map_or(start, |(d, _)| start.min(*d));

    // Fetch prices for all securities in the portfolio
    let symbols: Vec<&str> = portfolio.securities.keys().map(|s| s.as_str()).collect();
    let prices = price_service.get_prices(&symbols, fetch_start, end).await?;

    portfolio.generate_daily_statements(start, end, &prices, reporting_currency);

    let display = PortfolioDisplay {
        portfolio: &portfolio,
        sort_by: shared_args.sort_by,
        order,
        prices: &prices,
        reporting_currency,
    };
    println!("{}", display);

    Ok(())
}
