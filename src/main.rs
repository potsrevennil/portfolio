use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc; // Add Utc for current date
use clap::{builder::PossibleValue, Parser, Subcommand, ValueEnum};
use portfolio::prices::{PriceService, StockPriceStore}; // Add this line
use portfolio::{ib, Holding, Portfolio, Security};
use rust_decimal::{prelude::FromPrimitive, Decimal};
use unicode_width::UnicodeWidthStr;

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
    #[arg(short, long, default_value = "ib_combined.csv")]
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
        let name_width = UnicodeWidthStr::width(name);
        let padding = if name_width <= name_col_width { name_col_width - name_width } else { 0 };
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
    prices: &HashMap<String, Vec<portfolio::prices::StockPrice>>,
) {
    // Add prices map
    let order = match args.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(), // clap should prevent this
    };

    let total_holdings_market_value: Decimal =
        portfolio.holdings.values().map(|h| h.market_value).sum(); // Changed to market_value

    let total_cash: Decimal = portfolio.cash_balances.values().sum();
    let total_assets_usd = total_holdings_market_value + total_cash; // Changed to market_value

    let total_unrealized_pnl: Decimal =
        portfolio.holdings.values().map(|h| h.unrealized_pnl_value).sum();
    let total_realized_pnl: Decimal =
        portfolio.holdings.values().map(|h| h.realized_pnl_value).sum();

    let total_unrealized_pnl_percentage =
        (total_unrealized_pnl.checked_div(total_assets_usd).unwrap_or_default())
            * Decimal::from(100);
    let total_realized_pnl_percentage =
        (total_realized_pnl.checked_div(total_assets_usd).unwrap_or_default()) * Decimal::from(100);

    println!("--- Account Summary ---");
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

    let mut holdings_vec: Vec<_> = portfolio.holdings.iter().collect();

    if args.group {
        println!("--- All Holdings ---");
        print_holdings(
            &mut holdings_vec,
            &portfolio.securities,
            total_assets_usd,
            args.sort_by,
            order,
            prices, // Pass prices
        );
        println!("\n");
    } else {
        print_holdings(
            &mut holdings_vec,
            &portfolio.securities,
            total_assets_usd,
            args.sort_by,
            order,
            prices, // Pass prices
        );
    }

    println!("--- Cash Balances ---\n");
    for (currency, balance) in &portfolio.cash_balances {
        println!("{:?}: {:.2}", currency, balance);
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

            // Fetch prices for all securities in the portfolio
            let unique_symbols: Vec<&str> =
                portfolio.securities.keys().map(|s| s.as_str()).collect();
            let today = Utc::now().date_naive();
            let prices = price_service.get_prices(&unique_symbols, today, today).await?;

            portfolio.calculate_holdings(&prices); // Pass prices to calculate_holdings
            print_portfolio(&portfolio, &init_args.args, &prices);
        }
        None => {
            // Default to calculate
            portfolio.load_from_csv(&cli.calculate.transactions_file)?;

            // Fetch prices for all securities in the portfolio
            let unique_symbols: Vec<&str> =
                portfolio.securities.keys().map(|s| s.as_str()).collect();
            let today = Utc::now().date_naive();
            let prices = price_service.get_prices(&unique_symbols, today, today).await?;

            portfolio.calculate_holdings(&prices); // Pass prices to calculate_holdings
            print_portfolio(&portfolio, &cli.calculate.args, &prices);
        }
    }

    Ok(())
}
