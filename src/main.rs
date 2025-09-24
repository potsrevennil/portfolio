use std::collections::HashMap;

use anyhow::Result;
use chrono::{Duration, Months, NaiveDate, Utc};
use clap::{builder::PossibleValue, Parser, Subcommand, ValueEnum};
use portfolio::{
    ib,
    prices::{PriceService, StockPriceStore},
    Currency, Holding, Portfolio, Security,
};
use rust_decimal::{prelude::FromPrimitive, Decimal};

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

#[derive(ValueEnum, Clone, Debug, Copy)]
enum SortBy {
    Name,
    Percentage,
}

#[derive(Debug, Copy, Clone)]
enum Order {
    Asc,
    Desc,
}

fn print_holdings(
    holdings: &mut [(&String, &Holding)],
    securities: &HashMap<String, Security>,
    total_market_value_usd: Decimal, // Changed from total_assets_usd
    sort_by: SortBy,
    order: Order,
    prices: &HashMap<String, Vec<portfolio::prices::StockPrice>>, // Add prices map
) {
    match (sort_by, order) {
        (SortBy::Name, Order::Asc) => holdings.sort_by(|a, b| a.0.cmp(b.0)),
        (SortBy::Name, Order::Desc) => holdings.sort_by(|a, b| b.0.cmp(a.0)),
        (SortBy::Percentage, Order::Asc) => {
            holdings.sort_by(|a, b| a.1.market_value.partial_cmp(&b.1.market_value).unwrap())
            // Changed to market_value
        }
        (SortBy::Percentage, Order::Desc) => {
            holdings.sort_by(|a, b| b.1.market_value.partial_cmp(&a.1.market_value).unwrap())
            // Changed to market_value
        }
    }

    let name_col_width = 35;
    println!(
        "{:<15} {:<width$} {:>12} {:>18} {:>18} {:>15} {:>15} {:>15} {:>15} {:>14} {:>12}", /* Added {:>12} for Market Price */
        "Ticker",
        "Name",
        "Quantity",
        "Cost (USD)",
        "Market Value",
        "Unrealized P&L",
        "Unrealized %",
        "Realized P&L",
        "Realized %",
        "Portfolio %",
        "Mkt Price", // New column header
        width = name_col_width
    );
    println!("{}", "=".repeat(190)); // Adjusted width

    for (symbol, holding) in holdings {
        let percentage =
            holding.market_value.checked_div(total_market_value_usd).unwrap_or_default()
                * Decimal::from(100);
        let name = securities.get(*symbol).map_or("", |s| &s.description);
        let name_width = unicode_width::UnicodeWidthStr::width(name);
        let padding = name_col_width.saturating_sub(name_width);
        let name_part = format!("{}{}", name, " ".repeat(padding));

        let market_price = prices
            .get(*symbol)
            .and_then(|p| p.last()) // Get the latest price (assuming sorted by date)
            .map_or(Decimal::ZERO, |p| Decimal::from_f64(p.close_price).unwrap_or_default());

        println!(
            "{:<15} {} {:>12.4} {:>18.2} {:>18.2} {:>15.2} {:>14.2}% {:>15.2} {:>14.2}% {:>13.2}% \
             {:>11.2}", // Added {:>11.2} for Market Price
            symbol,
            name_part,
            holding.quantity,
            holding.total_cost,
            holding.market_value,
            holding.unrealized_pnl_value,
            holding.unrealized_pnl_percentage,
            holding.realized_pnl_value,
            holding.realized_pnl_percentage,
            percentage,
            market_price, // Use direct market price
        );
    }
}

fn print_portfolio(
    portfolio: &Portfolio,
    args: &SharedArgs,
    _end_date: NaiveDate, // end_date is now implicitly handled by daily_snapshots
    prices: &HashMap<String, Vec<portfolio::prices::StockPrice>>,
) {
    let order = match args.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(), // clap should prevent this
    };

    if portfolio.daily_snapshots.is_empty() {
        println!("No portfolio data available for the selected period.");
        return;
    }

    let mut sorted_snapshots: Vec<(
        &NaiveDate,
        &(HashMap<String, Holding>, HashMap<Currency, Decimal>),
    )> = portfolio.daily_snapshots.iter().collect();
    sorted_snapshots.sort_by_key(|(date, _)| *date);

    for (date, (holdings_map, cash_balances_map)) in sorted_snapshots {
        println!("\n======================================================================================================================================================================================================");
        println!("--- Account Summary as of {} ---", date);

        let total_holdings_market_value: Decimal =
            holdings_map.values().map(|h| h.market_value).sum();
        let total_cash: Decimal = cash_balances_map.values().sum();
        let total_assets_usd = total_holdings_market_value + total_cash;

        let total_unrealized_pnl: Decimal =
            holdings_map.values().map(|h| h.unrealized_pnl_value).sum();
        let total_realized_pnl: Decimal = holdings_map.values().map(|h| h.realized_pnl_value).sum();

        let total_unrealized_pnl_percentage =
            total_unrealized_pnl.checked_div(total_assets_usd).unwrap_or_default()
                * Decimal::from(100);
        let total_realized_pnl_percentage =
            total_realized_pnl.checked_div(total_assets_usd).unwrap_or_default()
                * Decimal::from(100);

        println!("{:<25}: {:>10.2} USD", "Total Portfolio Value", total_assets_usd);
        println!(
            "{:<25}: {:>10.2} USD ({:>6.2}%)",
            "Total Unrealized P&L", total_unrealized_pnl, total_unrealized_pnl_percentage
        );
        println!(
            "{:<25}: {:>10.2} USD ({:>6.2}%)",
            "Total Realized P&L", total_realized_pnl, total_realized_pnl_percentage
        );
        println!();

        let mut holdings_vec: Vec<_> = holdings_map.iter().collect();
        if args.group {
            println!("--- All Holdings ---");
            print_holdings(
                &mut holdings_vec,
                &portfolio.securities,
                total_assets_usd,
                args.sort_by,
                order,
                prices,
            );
            println!("\n");
        } else {
            print_holdings(
                &mut holdings_vec,
                &portfolio.securities,
                total_assets_usd,
                args.sort_by,
                order,
                prices,
            );
        }

        println!("--- Cash Balances ---\n");
        for (currency, balance) in cash_balances_map {
            println!("{:?}: {:.2}", currency, balance);
        }
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let cli = Cli::parse();

    let mut portfolio = Portfolio::new();

    // Initialize PriceService and StockPriceStore
    let pool = portfolio::db::init_db("sqlite:sqlite.db").await?; // Assuming sqlite.db is the default
    let price_store = StockPriceStore::new(pool.clone());
    let price_service = PriceService::new(price_store.clone());

    match cli.command {
        Some(Command::Init(init_args)) => {
            ib::load_from_ib_csv(&mut portfolio, &init_args.ib_file)?;
            portfolio.to_csv_file(&init_args.transactions_file)?;

            let end_date = Utc::now().date_naive();
            let start_date = if let Some(ref from) = init_args.args.from {
                match from {
                    TimeSelector::Year(y) => {
                        end_date.checked_sub_months(Months::new(*y * 12)).unwrap_or(NaiveDate::MIN)
                    }
                    TimeSelector::Month(m) => {
                        end_date.checked_sub_months(Months::new(*m)).unwrap_or(NaiveDate::MIN)
                    }
                    TimeSelector::Week(w) => end_date
                        .checked_sub_signed(Duration::weeks((*w).into()))
                        .unwrap_or(NaiveDate::MIN),
                    TimeSelector::Day(d) => end_date
                        .checked_sub_signed(Duration::days((*d).into()))
                        .unwrap_or(NaiveDate::MIN),
                    TimeSelector::Date(d) => *d,
                }
            } else {
                end_date
            };

            // Fetch prices for all securities in the portfolio
            let unique_symbols: Vec<&str> =
                portfolio.securities.keys().map(|s| s.as_str()).collect();
            let prices = price_service.get_prices(&unique_symbols, start_date, end_date).await?;

            portfolio.calculate_holdings(start_date, end_date, &prices); // Pass start_date, end_date and prices to calculate_holdings
            print_portfolio(&portfolio, &init_args.args, end_date, &prices);
        }
        None => {
            // Default to calculate
            portfolio.load_from_csv(&cli.calculate.transactions_file)?;

            let end_date = Utc::now().date_naive();
            let start_date = if let Some(ref from) = cli.calculate.args.from {
                match from {
                    TimeSelector::Year(y) => {
                        end_date.checked_sub_months(Months::new(*y * 12)).unwrap_or(NaiveDate::MIN)
                    }
                    TimeSelector::Month(m) => {
                        end_date.checked_sub_months(Months::new(*m)).unwrap_or(NaiveDate::MIN)
                    }
                    TimeSelector::Week(w) => end_date
                        .checked_sub_signed(Duration::weeks((*w).into()))
                        .unwrap_or(NaiveDate::MIN),
                    TimeSelector::Day(d) => end_date
                        .checked_sub_signed(Duration::days((*d).into()))
                        .unwrap_or(NaiveDate::MIN),
                    TimeSelector::Date(d) => *d,
                }
            } else {
                end_date
            };

            // Fetch prices for all securities in the portfolio
            let unique_symbols: Vec<&str> =
                portfolio.securities.keys().map(|s| s.as_str()).collect();
            let prices = price_service.get_prices(&unique_symbols, start_date, end_date).await?;

            portfolio.calculate_holdings(start_date, end_date, &prices); // Pass
                                                                         // start_date,
                                                                         // end_date
                                                                         // and prices
                                                                         // to calculate_holdings

            print_portfolio(&portfolio, &cli.calculate.args, end_date, &prices);
        }
    }

    Ok(())
}
