mod stocks;

use crate::stocks::{Asset, AssetType};
use clap::{builder::PossibleValue, Parser, ValueEnum};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

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

fn print_assets(assets: &mut [Asset], total_assets_usd: f64, sort_by: SortBy, order: Order) {
    match (sort_by, order) {
        (SortBy::Name, Order::Asc) => assets.sort_by(|a, b| a.ticker.cmp(&b.ticker)),
        (SortBy::Name, Order::Desc) => assets.sort_by(|a, b| b.ticker.cmp(&a.ticker)),
        (SortBy::Percentage, Order::Asc) => {
            assets.sort_by(|a, b| a.value_usd.partial_cmp(&b.value_usd).unwrap())
        }
        (SortBy::Percentage, Order::Desc) => {
            assets.sort_by(|a, b| b.value_usd.partial_cmp(&a.value_usd).unwrap())
        }
    }

    let name_col_width = 40;
    println!(
        "{: <15} {:<width$} {:<20} {:<10}",
        "Ticker",
        "Name",
        "Value (USD)",
        "Percentage",
        width = name_col_width
    );
    println!("{}", "=".repeat(90));

    for asset in assets {
        let percentage = (asset.value_usd / total_assets_usd) * 100.0;
        let name_width = UnicodeWidthStr::width(asset.name.as_str());
        let padding = if name_width <= name_col_width {
            name_col_width - name_width
        } else {
            0
        };
        let name_part = format!("{}{}", asset.name, " ".repeat(padding));
        println!(
            "{: <15} {} ${:<19.2} {:.2}%",
            asset.ticker,
            name_part,
            asset.value_usd,
            percentage
        );
    }
}

fn main() {
    let cli = Cli::parse();
    let mut assets: Vec<Asset> = stocks::get_all_assets();
    let total_assets_usd: f64 = assets.iter().map(|a| a.value_usd).sum();

    let order = match cli.order.as_str() {
        "+" => Order::Asc,
        "-" => Order::Desc,
        _ => unreachable!(), // clap should prevent this
    };

    println!("Total Portfolio Value: ${:.2} USD", total_assets_usd);
    println!("USD/TWD Exchange Rate: {}", stocks::USD_TO_TWD);
    println!();

    if cli.group {
        let mut grouped_assets: HashMap<AssetType, Vec<Asset>> = HashMap::new();
        for asset in assets {
            grouped_assets
                .entry(asset.asset_type.clone())
                .or_default()
                .push(asset);
        }

        let group_order = vec![AssetType::UsStock, AssetType::TwStock, AssetType::Crypto];

        for asset_type in group_order {
            if let Some(mut assets) = grouped_assets.remove(&asset_type) {
                println!("--- {} ---", asset_type);
                print_assets(&mut assets, total_assets_usd, cli.sort_by, order);
                println!();
            }
        }
    } else {
        print_assets(&mut assets, total_assets_usd, cli.sort_by, order);
    }
}
