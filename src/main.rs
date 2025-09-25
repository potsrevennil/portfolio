use anyhow::Result;
use chrono::{Duration, Months, NaiveDate, Utc};
use clap::{builder::PossibleValue, Parser, Subcommand};
use portfolio::{
    ib,
    prices::{PriceService, StockPriceStore},
    Order, Portfolio, SortBy,
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
    /// Initialize and process Interactive Brokers data
    Init(Init),
}

#[derive(Parser, Debug)]
struct Init {
    #[arg(short, long, default_value = "ib.csv")]
    ib_file: String,

    #[arg(short, long, default_value = "transactions.csv")]
    transactions_file: String,

    #[command(flatten)]
    args: SharedArgs,
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
    let pool = portfolio::db::init_db("sqlite:sqlite.db").await?;
    let price_store = StockPriceStore::new(pool.clone());
    let price_service = PriceService::new(price_store.clone());

    let shared_args = match cli.command {
        Some(Command::Init(init_args)) => {
            ib::load_from_ib_csv(&mut portfolio, &init_args.ib_file)?;
            portfolio.to_csv_file(&init_args.transactions_file)?;
            init_args.args
        }
        None => {
            portfolio.load_from_csv(&cli.calculate.transactions_file)?;
            cli.calculate.args
        }
    };

    let end_date = Utc::now().date_naive();
    let start_date = if let Some(ref from) = shared_args.from {
        match from {
            TimeSelector::Year(y) => {
                end_date.checked_sub_months(Months::new(*y * 12)).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Month(m) => {
                end_date.checked_sub_months(Months::new(*m)).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Week(w) => {
                end_date.checked_sub_signed(Duration::weeks((*w).into())).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Day(d) => {
                end_date.checked_sub_signed(Duration::days((*d).into())).unwrap_or(NaiveDate::MIN)
            }
            TimeSelector::Date(d) => *d,
        }
    } else {
        end_date
    };

    let order = match shared_args.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(),
    };

    portfolio
        .generate_report(start_date, end_date, shared_args.sort_by, order, &price_service)
        .await?;

    Ok(())
}
