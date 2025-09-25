use std::{collections::HashMap, fmt};

use chrono::NaiveDate;
use clap::ValueEnum;
use portfolio::{
    prices::StockPrice,
    stocks::{Currency, Holding, Portfolio, Security},
};
use rust_decimal::{prelude::FromPrimitive, Decimal};

#[derive(ValueEnum, Clone, Debug, Copy)]
pub enum SortBy {
    Name,
    Percentage,
}

#[derive(Debug, Copy, Clone)]
pub enum Order {
    Asc,
    Desc,
}

pub struct PortfolioDisplay<'a> {
    pub portfolio: &'a Portfolio,
    pub sort_by: SortBy,
    pub order: Order,
    pub prices: &'a HashMap<String, Vec<StockPrice>>,
}

impl fmt::Display for PortfolioDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let portfolio = self.portfolio;
        let sort_by = self.sort_by;
        let order = self.order;
        let prices = self.prices;

        if portfolio.daily_snapshots.is_empty() {
            writeln!(f, "No portfolio data available for the selected period.")?;
            return Ok(());
        }

        let mut sorted_snapshots: Vec<(
            &NaiveDate,
            &(HashMap<String, Holding>, HashMap<Currency, Decimal>),
        )> = portfolio.daily_snapshots.iter().collect();
        sorted_snapshots.sort_by_key(|(date, _)| *date);

        for (date, (holdings_map, cash_balances_map)) in sorted_snapshots {
            writeln!(f, "\n======================================================================================================================================================================================================")?;
            writeln!(f, "--- Account Summary as of {} ---", date)?;

            let total_holdings_market_value: Decimal =
                holdings_map.values().map(|h| h.market_value).sum();
            let total_cash: Decimal = cash_balances_map.values().sum();
            let total_assets_usd = total_holdings_market_value + total_cash;

            let total_unrealized_pnl: Decimal =
                holdings_map.values().map(|h| h.unrealized_pnl_value).sum();
            let total_realized_pnl: Decimal =
                holdings_map.values().map(|h| h.realized_pnl_value).sum();

            let total_unrealized_pnl_percentage =
                total_unrealized_pnl.checked_div(total_assets_usd).unwrap_or_default()
                    * Decimal::from(100);
            let total_realized_pnl_percentage =
                total_realized_pnl.checked_div(total_assets_usd).unwrap_or_default()
                    * Decimal::from(100);

            writeln!(f, "{:<25}: {:>10.2} USD", "Total Portfolio Value", total_assets_usd)?;
            writeln!(
                f,
                "{:<25}: {:>10.2} USD ({:>6.2}%)",
                "Total Unrealized P&L", total_unrealized_pnl, total_unrealized_pnl_percentage
            )?;
            writeln!(
                f,
                "{:<25}: {:>10.2} USD ({:>6.2}%)",
                "Total Realized P&L", total_realized_pnl, total_realized_pnl_percentage
            )?;
            writeln!(f, "")?;

            let mut holdings_vec: Vec<_> = holdings_map.iter().collect();
            match (sort_by, order) {
                (SortBy::Name, Order::Asc) => holdings_vec.sort_by(|a, b| a.0.cmp(b.0)),
                (SortBy::Name, Order::Desc) => holdings_vec.sort_by(|a, b| b.0.cmp(a.0)),
                (SortBy::Percentage, Order::Asc) => holdings_vec
                    .sort_by(|a, b| a.1.market_value.partial_cmp(&b.1.market_value).unwrap()),
                (SortBy::Percentage, Order::Desc) => holdings_vec
                    .sort_by(|a, b| b.1.market_value.partial_cmp(&a.1.market_value).unwrap()),
            }

            let name_col_width = 35;
            writeln!(
                f,
                "{:<15} {:<width$} {:>12} {:>18} {:>18} {:>15} {:>15} {:>15} {:>15} {:>14} {:>12}",
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
                "Mkt Price",
                width = name_col_width
            )?;
            writeln!(f, "{}", "=".repeat(190))?;

            for (symbol, holding) in holdings_vec {
                let holding_display = HoldingDisplay {
                    symbol,
                    holding,
                    securities: &portfolio.securities,
                    total_market_value_usd: total_assets_usd,
                    prices,
                    name_col_width,
                };
                writeln!(f, "{}", holding_display)?;
            }
            writeln!(f, "--- Cash Balances ---\n")?;
            for (currency, balance) in cash_balances_map {
                writeln!(f, "{:?}: {:.2}", currency, balance)?;
            }
        }
        Ok(())
    }
}

pub struct HoldingDisplay<'a> {
    pub symbol: &'a String,
    pub holding: &'a Holding,
    pub securities: &'a HashMap<String, Security>,
    pub total_market_value_usd: Decimal,
    pub prices: &'a HashMap<String, Vec<StockPrice>>,
    pub name_col_width: usize,
}

impl fmt::Display for HoldingDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let percentage =
            self.holding.market_value.checked_div(self.total_market_value_usd).unwrap_or_default()
                * Decimal::from(100);
        let name = self.securities.get(self.symbol).map_or("", |s| &s.description);
        let name_width = unicode_width::UnicodeWidthStr::width(name);
        let padding = self.name_col_width.saturating_sub(name_width);
        let name_part = format!("{}{}", name, " ".repeat(padding));

        let market_price = self.prices
            .get(self.symbol)
            .and_then(|p| p.last()) // Get the latest price (assuming sorted by date)
            .map_or(Decimal::ZERO, |p| Decimal::from_f64(p.close_price).unwrap_or_default());

        write!(
            f,
            "{:<15} {} {:>12.4} {:>18.2} {:>18.2} {:>15.2} {:>14.2}% {:>15.2} {:>14.2}% {:>13.2}% \
             {:>11.2}", // Added {:>11.2} for Market Price
            self.symbol,
            name_part,
            self.holding.quantity,
            self.holding.total_cost,
            self.holding.market_value,
            self.holding.unrealized_pnl_value,
            self.holding.unrealized_pnl_percentage,
            self.holding.realized_pnl_value,
            self.holding.realized_pnl_percentage,
            percentage,
            market_price, // Use direct market price
        )
    }
}

pub(crate) fn print_portfolio(
    portfolio: &Portfolio,
    sort_by: SortBy,
    order: Order,
    _end_date: NaiveDate, // end_date is now implicitly handled by daily_snapshots
    prices: &HashMap<String, Vec<portfolio::prices::StockPrice>>, // Add prices map
) {
    let portfolio_display = PortfolioDisplay { portfolio, sort_by, order, prices };
    println!("{}", portfolio_display);
}
