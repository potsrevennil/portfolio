mod ib;
mod stocks;

use std::collections::HashMap;

use clap::{builder::PossibleValue, Parser, ValueEnum};
use rust_decimal::Decimal;
use unicode_width::UnicodeWidthStr;

use crate::stocks::{Holding, Portfolio, Security};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
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
    group: bool, // This is not used yet in the refactored version
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
    let get_value = |h: &(&String, &Holding)| h.1.quantity * h.1.average_cost;

    match (sort_by, order) {
        (SortBy::Name, Order::Asc) => holdings.sort_by(|a, b| a.0.cmp(b.0)),
        (SortBy::Name, Order::Desc) => holdings.sort_by(|a, b| b.0.cmp(a.0)),
        (SortBy::Percentage, Order::Asc) => {
            holdings.sort_by(|a, b| get_value(a).partial_cmp(&get_value(b)).unwrap())
        }
        (SortBy::Percentage, Order::Desc) => {
            holdings.sort_by(|a, b| get_value(b).partial_cmp(&get_value(a)).unwrap())
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
            "{: <15} {} {:>12.4} {:>17.2} {:>14.2}%",
            symbol, name_part, holding.quantity, holding.total_cost, percentage
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let mut portfolio = Portfolio::new();
    ib::load_from_ib_csv(&mut portfolio, "ib_combined.csv")?;
    portfolio.calculate_holdings();
    portfolio.to_csv_file("transactions.csv")?;

    let order = match cli.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(), // clap should prevent this
    };

    let total_holdings_value: Decimal =
        portfolio.holdings.values().map(|h| h.quantity * h.average_cost).sum();

    let total_cash: Decimal = portfolio.cash_balances.values().sum(); // Assuming
    let total_assets_usd = total_holdings_value + total_cash;

    println!("Total Portfolio Value: ${:.2} USD", total_assets_usd);
    println!();

    let mut holdings_vec: Vec<_> = portfolio.holdings.iter().collect();

    if cli.group {
        // Grouping logic is not implemented yet as AssetType is gone.
        // For now, just print all holdings.
        println!("--- All Holdings ---");
        print_holdings(
            &mut holdings_vec,
            &portfolio.securities,
            total_assets_usd,
            cli.sort_by,
            order,
        );
        println!();
    } else {
        print_holdings(
            &mut holdings_vec,
            &portfolio.securities,
            total_assets_usd,
            cli.sort_by,
            order,
        );
    }

    println!("--- Cash Balances ---");
    for (currency, balance) in &portfolio.cash_balances {
        println!("{:?}: {:.2}", currency, balance);
    }

    Ok(())
}
