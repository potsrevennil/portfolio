use std::collections::HashMap;

use unicode_width::UnicodeWidthStr;

// Currency conversion rate
const USD_TO_TWD: f64 = 30.11;

// Your original data
fn get_us_stocks() -> HashMap<String, (&'static str, f64, f64, f64)> {
    let mut us_stocks = HashMap::new();
    us_stocks.insert("US01".to_string(), ("Example US 01", 1.0, 10.0, 10.0));
    us_stocks.insert("US02".to_string(), ("Example US 02", 1.0, 10.0, 10.0));
    us_stocks.insert("US03".to_string(), ("Example US 03", 1.0, 10.0, 10.0));
    us_stocks.insert("US04".to_string(), ("Example US 04", 1.0, 10.0, 10.0));
    us_stocks.insert("US05".to_string(), ("Example US 05", 1.0, 10.0, 10.0));
    us_stocks.insert("US06".to_string(), ("Example US 06", 1.0, 10.0, 10.0));
    us_stocks.insert("US07".to_string(), ("Example US 07", 1.0, 10.0, 10.0));
    us_stocks.insert("US08".to_string(), ("Example US 08", 1.0, 10.0, 10.0));
    us_stocks.insert("US09".to_string(), ("Example US 09", 1.0, 10.0, 10.0));
    us_stocks.insert("US10".to_string(), ("Example US 10", 1.0, 10.0, 10.0));
    us_stocks.insert("US11".to_string(), ("Example US 11", 1.0, 10.0, 10.0));
    us_stocks.insert("US12".to_string(), ("Example US 12", 1.0, 10.0, 10.0));
    us_stocks.insert("US13".to_string(), ("Example US 13", 1.0, 10.0, 10.0));
    us_stocks.insert("US14".to_string(), ("Example US 14", 1.0, 10.0, 10.0));
    us_stocks.insert("US15".to_string(), ("Example US 15", 1.0, 10.0, 10.0));
    us_stocks.insert("US16".to_string(), ("Example US 16", 1.0, 10.0, 10.0));
    us_stocks
}

fn get_tw_stocks() -> HashMap<String, (&'static str, f64, f64, f64)> {
    let mut tw_stocks = HashMap::new();
    tw_stocks.insert("ZZ01".to_string(), ("範例證券01", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ09".to_string(), ("範例證券09", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ08".to_string(), ("範例證券08", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ11".to_string(), ("範例證券11", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ13".to_string(), ("範例證券13", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ16".to_string(), ("範例證券16", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ14".to_string(), ("範例證券14", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ12".to_string(), ("範例證券12", 1.0, 10.0, 10.0));
    tw_stocks.insert("ZZ15".to_string(), ("範例證券15", 1.0, 10.0, 10.0));
    tw_stocks
}

fn get_crypto() -> HashMap<String, (&'static str, f64, f64)> {
    let mut crypto = HashMap::new();
    crypto.insert("COIN01".to_string(), ("Example Coin 01", 1.0, 10.0));
    crypto.insert("COIN02".to_string(), ("Example Coin 02", 1.0, 10.0));
    crypto.insert("COIN03".to_string(), ("Example Coin 03", 1.0, 10.0));
    crypto.insert("COIN04".to_string(), ("Example Coin 04", 1.0, 10.0));
    crypto.insert("COIN05".to_string(), ("Example Coin 05", 1.0, 10.0));
    crypto
}

struct Asset {
    ticker: String,
    name: String,
    value_usd: f64,
}

fn main() {
    let us_stocks = get_us_stocks();
    let tw_stocks = get_tw_stocks();
    let crypto = get_crypto();

    let mut assets = Vec::new();

    for (ticker, (name, amount, _, current_price)) in us_stocks {
        assets.push(Asset {
            ticker: ticker.clone(),
            name: name.to_string(),
            value_usd: amount * current_price,
        });
    }

    for (ticker, (name, amount, _, current_price)) in tw_stocks {
        assets.push(Asset {
            ticker: ticker.clone(),
            name: name.to_string(),
            value_usd: (amount * current_price) / USD_TO_TWD,
        });
    }

    for (ticker, (name, amount, current_price)) in crypto {
        assets.push(Asset {
            ticker: ticker.clone(),
            name: name.to_string(),
            value_usd: amount * current_price,
        });
    }

    let total_assets_usd: f64 = assets.iter().map(|a| a.value_usd).sum();

    println!("Total Portfolio Value: ${:.2} USD", total_assets_usd);
    println!("USD/TWD Exchange Rate: {}", USD_TO_TWD);
    println!();

    let name_col_width = 40;
    println!(
        "{:<15} {:<width$} {:<20} {:<10}",
        "Ticker",
        "Name",
        "Value (USD)",
        "Percentage",
        width = name_col_width
    );
    println!("{}", "=".repeat(90));

    assets.sort_by(|a, b| b.value_usd.partial_cmp(&a.value_usd).unwrap());

    for asset in assets {
        let percentage = (asset.value_usd / total_assets_usd) * 100.0;
        let name_width = UnicodeWidthStr::width(asset.name.as_str());
        let padding = if name_width <= name_col_width { name_col_width - name_width } else { 0 };
        let name_part = format!("{}{}", asset.name, " ".repeat(padding));
        println!(
            "{:<15} {} ${:<19.2} {:.2}%",
            asset.ticker, name_part, asset.value_usd, percentage
        );
    }
}
