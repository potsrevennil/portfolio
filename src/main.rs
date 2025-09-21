mod ib;
mod stocks;

use std::collections::HashMap;

use anyhow::Result;
use clap::{builder::PossibleValue, Parser, Subcommand, ValueEnum};
use rust_decimal::Decimal;
use unicode_width::UnicodeWidthStr;

use crate::stocks::{Holding, Portfolio, Security};

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
    total_assets_usd: Decimal,
    sort_by: SortBy,
    order: Order,
) {
    match (sort_by, order) {
        (SortBy::Name, Order::Asc) => holdings.sort_by(|a, b| a.0.cmp(b.0)),
        (SortBy::Name, Order::Desc) => holdings.sort_by(|a, b| b.0.cmp(a.0)),
        (SortBy::Percentage, Order::Asc) => {
            holdings.sort_by(|a, b| a.1.total_cost.partial_cmp(&b.1.total_cost).unwrap())
        }
        (SortBy::Percentage, Order::Desc) => {
            holdings.sort_by(|a, b| b.1.total_cost.partial_cmp(&a.1.total_cost).unwrap())
        }
    }

    let name_col_width = 35;
    println!(
        "{: <15} {:<width$} {:>12} {:>18} {:>15}",
        "Ticker",
        "Name",
        "Quantity",
        "Cost (USD)",
        "Percentage",
        width = name_col_width
    );
    println!("{}", "=".repeat(99));

    for (symbol, holding) in holdings {
        let percentage = holding.total_cost.checked_div(total_assets_usd).unwrap_or_default()
            * Decimal::from(100);
        let name = securities.get(*symbol).map_or("", |s| &s.description);
        let name_width = UnicodeWidthStr::width(name);
        let padding = if name_width <= name_col_width { name_col_width - name_width } else { 0 };
        let name_part = format!("{}{}", name, " ".repeat(padding));
        println!(
            "{: <15} {} {:>12.4} {:>18.2} {:>14.2}%",
            symbol, name_part, holding.quantity, holding.total_cost, percentage
        );
    }
}

fn print_portfolio(portfolio: &Portfolio, args: &SharedArgs) {
    let order = match args.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(), // clap should prevent this
    };

    let total_holdings_value: Decimal = portfolio.holdings.values().map(|h| h.total_cost).sum();

    let total_cash: Decimal = portfolio.cash_balances.values().sum();
    let total_assets_usd = total_holdings_value + total_cash;

    println!("Total Portfolio Value: ${:.2} USD", total_assets_usd);
    println!();

    let mut holdings_vec: Vec<_> = portfolio.holdings.iter().collect();

    if args.group {
        // Grouping logic is not implemented yet as AssetType is gone.
        // For now, just print all holdings.
        println!("--- All Holdings ---");
        print_holdings(
            &mut holdings_vec,
            &portfolio.securities,
            total_assets_usd,
            args.sort_by,
            order,
        );
        println!();
    } else {
        print_holdings(
            &mut holdings_vec,
            &portfolio.securities,
            total_assets_usd,
            args.sort_by,
            order,
        );
    }

    println!("--- Cash Balances ---");
    for (currency, balance) in &portfolio.cash_balances {
        println!("{:?}: {:.2}", currency, balance);
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut portfolio = Portfolio::new();

    match cli.command {
        Some(Command::Init(init_args)) => {
            ib::load_from_ib_csv(&mut portfolio, &init_args.ib_file)?;
            portfolio.to_csv_file(&init_args.transactions_file)?;
            portfolio.calculate_holdings();
            print_portfolio(&portfolio, &init_args.args);
        }
        None => {
            // Default to calculate
            portfolio.load_from_csv(&cli.calculate.transactions_file)?;
            portfolio.calculate_holdings();
            print_portfolio(&portfolio, &cli.calculate.args);
        }
    }

    Ok(())
}
