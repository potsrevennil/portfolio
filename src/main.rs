use anyhow::Result;
use chrono::{Duration, Months, NaiveDate, Utc};
use clap::{builder::PossibleValue, Parser};
use portfolio::{
    db,
    event::{self, loader::DataSource},
    portfolio::{
        consolidated::{ConsolidatedPortfolio, ConsolidatedPortfolioDisplay},
        Currency, Portfolio,
    },
    prices::PriceService,
    record,
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
    Record(Record),
}

#[derive(Parser, Debug)]
struct Record {
    #[arg(short, long, default_value = "manual_transactions.csv")]
    output_file: String,
}

#[derive(Parser, Debug, Clone)]
struct Init {
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
struct Calculate {
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

    #[arg(long, value_enum, default_value_t = Currency::USD)]
    reporting_currency: Currency,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match &cli.command {
        Some(Command::Record(record_args)) => {
            record::run_interactive_record_session(&record_args.output_file).await
        }
        Some(Command::Init(_)) | None => run_calculation(&cli).await,
    }
}

async fn run_calculation(cli: &Cli) -> Result<()> {
    let pool = db::init_db("sqlite:sqlite.db").await?;
    let price_service = PriceService::new(pool.clone());
    let split_store = SplitStore::new(pool.clone());

    let (shared_args, sources, output_file) = if let Some(Command::Init(init_args)) = &cli.command {
        let sources = vec![
            DataSource::Ib(init_args.ib_files.clone()),
            DataSource::Cathay(init_args.cathay_files.clone()),
            DataSource::Generic(init_args.transactions_files.clone()),
        ];
        (init_args.args.clone(), sources, Some(init_args.output_file.clone()))
    } else {
        (
            cli.calculate_args.args.clone(),
            vec![DataSource::Generic(cli.calculate_args.transactions_files.clone())],
            None,
        )
    };

    let broker_data = event::load(sources, output_file, &split_store).await?;
    let mut portfolios = ConsolidatedPortfolio::from(
        broker_data.into_iter().map(|(b, (es, ss))| (b, Portfolio::new(b, es, ss))).collect(),
    );

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

    let prices =
        portfolios.get_prices(shared_args.reporting_currency, start, end, &price_service).await?;

    portfolios.generate_daily_statements(start, end, &prices);

    portfolios.calculate_totals(shared_args.reporting_currency, &prices);

    let order = match shared_args.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(),
    };

    let display = ConsolidatedPortfolioDisplay {
        consolidated_portfolio: &portfolios,
        sort_by: shared_args.sort_by,
        order,
        prices: &prices,
    };

    println!("{}", display);

    Ok(())
}
