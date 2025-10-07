use std::{
    collections::{HashMap, HashSet},
    fmt,
    ops::{Deref, DerefMut},
};

use anyhow::Result;
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::{
    portfolio::portfolio::{Broker, Currency, Portfolio, PortfolioDisplay},
    prices::{source::YFinanceSource, PriceService, StockPrice},
    Order, SortBy,
};

#[derive(Debug, Default)]
pub struct ConsolidatedPortfolio(HashMap<Broker, Portfolio>);

impl ConsolidatedPortfolio {
    pub fn new() -> Self { ConsolidatedPortfolio(HashMap::new()) }

    pub fn from(ps: HashMap<Broker, Portfolio>) -> Self { ConsolidatedPortfolio(ps) }

    pub async fn get_prices(
        &self,
        reporting_currency: Currency,
        start_date: NaiveDate,
        end_date: NaiveDate,
        price_service: &PriceService,
    ) -> Result<HashMap<String, Vec<StockPrice>>> {
        let mut all_symbols = HashSet::new();
        let mut currency_pairs = HashSet::new();

        for (b, p) in self.iter() {
            let broker_reporting_currency = b.reporting_currency();

            // Collect currency pairs from transactions
            for (_date, event) in p.events.iter() {
                for transaction in event.transactions.iter() {
                    currency_pairs.insert((transaction.currency, broker_reporting_currency));
                }
            }

            // Collect currency pair for portfolio reporting
            currency_pairs.insert((broker_reporting_currency, reporting_currency));

            all_symbols.extend(p.securities.keys().cloned());
        }

        // Generate exchange rate symbols
        for (from_currency, to_currency) in currency_pairs {
            if let Some(ticker) =
                YFinanceSource::get_exchange_rate_ticker(from_currency, to_currency)
            {
                all_symbols.insert(ticker);
            }
        }

        let fetch_start = self
            .values()
            .filter_map(|p| p.events.first_key_value())
            .map(|(d, _)| *d)
            .min()
            .map_or(start_date, |min_date| start_date.min(min_date));

        let symbols_ref: Vec<&str> = all_symbols.iter().map(|s| s.as_str()).collect();
        let prices = price_service.get_prices(&symbols_ref, fetch_start, end_date).await?;

        Ok(prices)
    }

    pub fn generate_daily_statements(
        &mut self,
        start_date: NaiveDate,
        end_date: NaiveDate,
        prices: &HashMap<String, Vec<StockPrice>>,
    ) {
        for portfolio in self.values_mut() {
            portfolio.generate_daily_statements(start_date, end_date, prices);
        }
    }
}

impl Deref for ConsolidatedPortfolio {
    type Target = HashMap<Broker, Portfolio>;

    fn deref(&self) -> &Self::Target { &self.0 }
}

impl DerefMut for ConsolidatedPortfolio {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

pub struct ConsolidatedPortfolioDisplay<'a> {
    pub consolidated_portfolio: &'a ConsolidatedPortfolio,
    pub sort_by: SortBy,
    pub order: Order,
    pub prices: &'a HashMap<String, Vec<StockPrice>>,
    pub reporting_currency: Currency,
}

impl fmt::Display for ConsolidatedPortfolioDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut total_consolidated_value = Decimal::ZERO;
        let mut total_consolidated_unrealized_pnl = Decimal::ZERO;
        let mut total_consolidated_realized_pnl = Decimal::ZERO;

        // Calculate consolidated totals
        for (broker, portfolio) in self.consolidated_portfolio.iter() {
            if let Some((_date, (holdings, cash_balances))) =
                portfolio.daily_statements.last_key_value()
            {
                let broker_reporting_currency = broker.reporting_currency();

                let mut broker_total_assets = Decimal::ZERO;
                let mut broker_total_unrealized_pnl = Decimal::ZERO;
                let mut broker_total_realized_pnl = Decimal::ZERO;

                // Sum holdings in broker's reporting currency
                for (_symbol, holding) in holdings.iter() {
                    broker_total_assets += holding.market_value;
                    broker_total_unrealized_pnl += holding.unrealized_pnl_value;
                    broker_total_realized_pnl += holding.realized_pnl_value;
                }

                // Sum cash balances in broker's reporting currency
                for (_currency, balance) in cash_balances.iter() {
                    broker_total_assets += balance;
                }

                // Convert broker totals to overall reporting currency
                let conversion_rate = YFinanceSource::get_conversion_rate(
                    broker_reporting_currency,
                    self.reporting_currency,
                    self.prices,
                );

                total_consolidated_value += broker_total_assets * conversion_rate;
                total_consolidated_unrealized_pnl += broker_total_unrealized_pnl * conversion_rate;
                total_consolidated_realized_pnl += broker_total_realized_pnl * conversion_rate;
            }
        }

        // Display Consolidated Summary
        writeln!(f, "======================================================================================================================================================================================================")?;
        writeln!(f, "--- Consolidated Portfolio Summary (in {}) ---", self.reporting_currency)?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Portfolio Value", total_consolidated_value, self.reporting_currency
        )?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Unrealized P&L", total_consolidated_unrealized_pnl, self.reporting_currency
        )?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Realized P&L", total_consolidated_realized_pnl, self.reporting_currency
        )?;
        writeln!(f, "")?;

        for (broker, portfolio) in self.consolidated_portfolio.iter() {
            writeln!(
                f,
                "

--- Portfolio for Broker: {} ---",
                broker
            )?;
            let display = PortfolioDisplay {
                portfolio,
                sort_by: self.sort_by,
                order: self.order,
                prices: self.prices,
                reporting_currency: broker.reporting_currency(),
            };
            write!(f, "{}", display)?;
        }
        Ok(())
    }
}
