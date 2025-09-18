use std::collections::HashMap;

use unicode_width::UnicodeWidthStr;

// Currency conversion rate
const USD_TO_TWD: f64 = 30.11;

// Your original data
fn get_us_stocks() -> HashMap<String, (&'static str, f64, f64, f64)> {
    let mut us_stocks = HashMap::new();
    us_stocks.insert("AAPL".to_string(), ("Apple", 24.0, 223.63, 239.38));
    us_stocks.insert("AMZN".to_string(), ("Amazon.com", 18.0, 210.41, 230.83));
    us_stocks.insert("GOOGL".to_string(), ("Alphabet", 23.0, 196.46, 248.05));
    us_stocks.insert("GTLB".to_string(), ("GitLab", 25.0, 50.88, 49.38));
    us_stocks.insert("IBKR".to_string(), ("Interactive Brokers Group", 7.3744, 51.3, 62.22));
    us_stocks.insert("LITE".to_string(), ("Lumentum Holdings", 3.0, 83.47, 164.6));
    us_stocks.insert("META".to_string(), ("Meta Platforms", 4.0, 631.13, 776.0));
    us_stocks.insert("MSFT".to_string(), ("Microsoft Corporation", 8.0, 442.92, 507.61));
    us_stocks.insert("NET".to_string(), ("Cloudflare", 5.0, 114.09, 210.37));
    us_stocks.insert("NFLX".to_string(), ("Netflix", 1.0, 928.66, 1217.64));
    us_stocks.insert("NVDA".to_string(), ("NVIDIA Corporation", 40.0, 140.72, 169.6));
    us_stocks.insert("SMR".to_string(), ("NuScale Power Corporation", 15.0, 44.43, 35.89));
    us_stocks.insert("SNOW".to_string(), ("Snowflake", 7.0, 168.91, 215.54));
    us_stocks.insert("TSLA".to_string(), ("Tesla", 5.0, 254.52, 418.57));
    us_stocks.insert("VOO".to_string(), ("Vanguard S&P 500 ETF", 10.0, 521.01, 605.8));
    us_stocks.insert("XLF".to_string(), ("Financial Select Sector SPDR Fund", 10.0, 53.37, 54.04));
    us_stocks
}

fn get_tw_stocks() -> HashMap<String, (&'static str, f64, f64, f64)> {
    let mut tw_stocks = HashMap::new();
    tw_stocks.insert("0050".to_string(), ("元大台灣50 ETF", 10200.0, 41.13, 56.35));
    tw_stocks.insert("00679B".to_string(), ("元大美債20年", 2106.0, 29.7, 26.76));
    tw_stocks.insert("00878".to_string(), ("國泰台灣ESG永續高股息 ETF", 13019.0, 20.96, 21.03));
    tw_stocks.insert("00891".to_string(), ("中信關鍵半導體", 43.0, 17.33, 18.42));
    tw_stocks.insert("00918".to_string(), ("大華優利高填息30", 4000.0, 24.35, 22.93));
    tw_stocks.insert("2330".to_string(), ("台灣積體電路製造股份有限公司", 32.0, 883.25, 1265.0));
    tw_stocks.insert("2392".to_string(), ("正崴精密工業股份有限公司", 4000.0, 75.66, 48.45));
    tw_stocks.insert("2646".to_string(), ("星宇航空股份有限公司", 5100.0, 27.62, 25.1));
    tw_stocks.insert("6786".to_string(), ("芯測股份有限公司", 1000.0, 26.35, 42.4));
    tw_stocks
}

fn get_crypto() -> HashMap<String, (&'static str, f64, f64)> {
    let mut crypto = HashMap::new();
    crypto.insert("BTC".to_string(), ("Bitcoin", 0.0355, 116645.0));
    crypto.insert("ETH".to_string(), ("Ethereum", 2.23644, 4610.0));
    crypto.insert("USDT(pionex)".to_string(), ("USDT (Pionex)", 50722.0, 1.0));
    crypto.insert("USDT(bitfinex)".to_string(), ("USDT (Bitfinex)", 33016.0, 1.0));
    crypto.insert("USD(IB)".to_string(), ("USD (Interactive Broker)", 389.13, 1.0));
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
