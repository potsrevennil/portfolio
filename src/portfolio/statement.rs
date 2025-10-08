use std::{collections::HashMap, fmt};

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::{
    portfolio::{
        holding::{Holding, HoldingDisplay},
        portfolio::{Currency, Order, Security, SortBy},
    },
    prices::{source::YFinanceSource, StockPrice},
};

#[derive(Debug, Default, Clone)]
pub struct Statement {
    pub holdings: HashMap<String, Holding>,
    pub cash_balances: HashMap<Currency, Decimal>,
    pub total_market_value: Decimal,
    pub total_realized_pnl_value: Decimal,
    pub total_unrealized_pnl_value: Decimal,
    pub total_cash_balance: Decimal,
    pub total_value: Decimal,
}

impl Statement {
    pub fn from(
        holdings: HashMap<String, Holding>,
        cash_balances: HashMap<Currency, Decimal>,
        prices: &HashMap<String, Vec<StockPrice>>,
        reporting_currency: Currency,
    ) -> Self {
        let mut total_market_value = Decimal::ZERO;
        let mut total_realized_pnl_value = Decimal::ZERO;
        let mut total_unrealized_pnl_value = Decimal::ZERO;
        let mut total_cash_balance = Decimal::ZERO;

        for holding in holdings.values() {
            let conversion_rate =
                YFinanceSource::get_conversion_rate(holding.currency, reporting_currency, prices);
            total_market_value += holding.market_value * conversion_rate;
            total_realized_pnl_value += holding.realized_pnl_value * conversion_rate;
            total_unrealized_pnl_value += holding.unrealized_pnl_value * conversion_rate;
        }

        for (currency, balance) in &cash_balances {
            let conversion_rate =
                YFinanceSource::get_conversion_rate(*currency, reporting_currency, prices);
            total_cash_balance += *balance * conversion_rate;
        }

        Self {
            holdings,
            cash_balances,
            total_market_value,
            total_realized_pnl_value,
            total_unrealized_pnl_value,
            total_cash_balance,
            total_value: total_market_value + total_cash_balance,
        }
    }
}

pub struct StatementDisplay<'a> {
    pub date: NaiveDate,
    pub statement: &'a Statement,
    pub securities: &'a HashMap<String, Security>,
    pub prices: &'a HashMap<String, Vec<StockPrice>>,
    pub sort_by: SortBy,
    pub order: Order,
    pub reporting_currency: Currency,
    pub total_consolidated_portfolio_value: Decimal,
    pub consolidated_reporting_currency: Currency,
}

impl fmt::Display for StatementDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let statement = self.statement;
        let holdings = &statement.holdings;
        let cash_balances = &statement.cash_balances;

        writeln!(f, "\n======================================================================================================================================================================================================")?;
        writeln!(f, "--- Account Summary as of {} ---", self.date)?;

        let total_value = statement.total_value;
        let total_unrealized_pnl = statement.total_unrealized_pnl_value;
        let total_realized_pnl = statement.total_realized_pnl_value;

        let total_unrealized_pnl_percentage =
            total_unrealized_pnl.checked_div(total_value).unwrap_or_default() * Decimal::from(100);

        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Portfolio Value", total_value, self.reporting_currency
        )?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {} ({:>6.2}%)",
            "Total Unrealized P&L",
            total_unrealized_pnl,
            self.reporting_currency,
            total_unrealized_pnl_percentage
        )?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Realized P&L", total_realized_pnl, self.reporting_currency,
        )?;
        writeln!(f, "")?;

        let mut holdings_vec: Vec<_> = holdings.iter().collect();
        match (self.sort_by, self.order) {
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
            "{:<15} {:<width$} {:>12} {:>18} {:>18} {:>15} {:>15} {:>15} {:>14} {:>12}",
            "Ticker",
            "Name",
            "Quantity",
            format!("Cost ({})", self.reporting_currency),
            "Market Value",
            "Unrealized P&L",
            "Unrealized %",
            "Realized P&L",
            "Portfolio %",
            "Mkt Price",
            width = name_col_width,
        )?;
        writeln!(f, "{}", "=".repeat(190))?;

        for (symbol, holding) in holdings_vec {
            let holding_display = HoldingDisplay {
                symbol,
                holding,
                securities: self.securities,
                prices: self.prices,
                total_consolidated_portfolio_value: self.total_consolidated_portfolio_value,
                consolidated_reporting_currency: self.consolidated_reporting_currency,
                name_col_width,
                reporting_currency: self.reporting_currency,
            };
            writeln!(f, "{}", holding_display)?;
        }
        writeln!(f, "")?;
        writeln!(f, "--- Cash Balance ---")?;
        for (currency, balance) in cash_balances {
            writeln!(
                f,
                "{:<25}: {:>10.2} {}",
                format!("Total Cash ({})", currency),
                balance,
                currency
            )?;
        }
        Ok(())
    }
}
