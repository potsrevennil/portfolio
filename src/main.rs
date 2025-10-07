use anyhow::Result;
use chrono::{Duration, Months, NaiveDate, Utc};
use clap::{builder::PossibleValue, Parser};
use portfolio::{
    db,
    event::{self, loader::DataSource},
    portfolio::{
        consolidated::{ConsolidatedPortfolio, ConsolidatedPortfolioDisplay},
        Broker, Currency, Portfolio,
    },
    prices::{PriceService, StockPriceStore},
    split::store::SplitStore,
    Order, SortBy,
};

#[derive(Debug, Clone)]
enum TimeSelector {
    Year(u32),
    Month(u32),
    Week(u32),
    Day(u32),
    Date(NaiveDate),
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
    calculate_args: Calculate,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Initialize and process broker data
    Init(Init),
}

#[derive(Parser, Debug)]
struct Init {
    /// One or more file paths for IB reports
    #[arg(long, num_args = 1..)]
    ib_files: Vec<String>,

    /// One or more file paths for Cathay reports
    #[arg(long, num_args = 1..)]
    cathay_files: Vec<String>,

    #[arg(short, long, default_value = "transactions.csv")]
    transactions_file: String,

    #[command(flatten)]
    args: SharedArgs,
}

#[derive(Parser, Debug)]
struct Calculate {
    #[arg(default_value = "transactions.csv", num_args = 1..)]
    transactions_files: Vec<String>,

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

    let pool = db::init_db("sqlite:sqlite.db").await?;
    let price_store = StockPriceStore::new(pool.clone());
    let price_service = PriceService::new(price_store.clone());
    let split_store = SplitStore::new(pool.clone());

    let (shared_args, sources, output_file) = if let Some(Command::Init(init_args)) = cli.command {
        let mut sources = vec![];
        if !init_args.ib_files.is_empty() {
            sources.push(DataSource::Ib(init_args.ib_files));
        }
        if !init_args.cathay_files.is_empty() {
            sources.push(DataSource::Cathay(init_args.cathay_files));
        }
        (init_args.args, sources, Some(init_args.transactions_file))
    } else {
        (
            cli.calculate_args.args,
            vec![DataSource::Generic(cli.calculate_args.transactions_files)],
            None,
        )
    };

    let broker_data = event::load(sources, output_file, &split_store).await?;
    let mut consolidated_portfolio = ConsolidatedPortfolio::new();
    let mut all_symbols = vec![];

    for (broker, (events, securities)) in broker_data {
        let mut portfolio = Portfolio::new(events, securities);
        portfolio.reporting_currency =
            if broker == Broker::Cathay { Currency::TWD } else { Currency::USD };
        all_symbols.extend(portfolio.securities.keys().cloned());
        consolidated_portfolio.insert(broker, portfolio);
    }

    all_symbols.sort();
    all_symbols.dedup();

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

    let fetch_start = consolidated_portfolio
        .values()
        .filter_map(|p| p.events.first_key_value())
        .map(|(d, _)| *d)
        .min()
        .map_or(start, |min_date| start.min(min_date));

    let symbols_ref: Vec<&str> = all_symbols.iter().map(|s| s.as_str()).collect();
    let prices = price_service.get_prices(&symbols_ref, fetch_start, end).await?;

    for portfolio in consolidated_portfolio.values_mut() {
        portfolio.generate_daily_statements(start, end, &prices);
    }

    let display = ConsolidatedPortfolioDisplay {
        consolidated_portfolio: &consolidated_portfolio,
        sort_by: shared_args.sort_by,
        order,
        prices: &prices,
    };

    println!("{}", display);

    Ok(())
}
