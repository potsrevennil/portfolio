mod stocks;

use unicode_width::UnicodeWidthStr;

use crate::stocks::Asset;

fn main() {
    let mut assets: Vec<Asset> = stocks::get_all_assets();

    let total_assets_usd: f64 = assets.iter().map(|a| a.value_usd).sum();

    println!("Total Portfolio Value: ${:.2} USD", total_assets_usd);
    println!("USD/TWD Exchange Rate: {}", stocks::USD_TO_TWD);
    println!();

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

    assets.sort_by(|a, b| b.value_usd.partial_cmp(&a.value_usd).unwrap());

    for asset in assets {
        let percentage = (asset.value_usd / total_assets_usd) * 100.0;
        let name_width = UnicodeWidthStr::width(asset.name.as_str());
        let padding = if name_width <= name_col_width { name_col_width - name_width } else { 0 };
        let name_part = format!("{}{}", asset.name, " ".repeat(padding));
        println!(
            "{: <15} {} ${:<19.2} {:.2}%",
            asset.ticker, name_part, asset.value_usd, percentage
        );
    }
}