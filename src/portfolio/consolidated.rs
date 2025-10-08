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
pub struct ConsolidatedPortfolio {
    portfolios: HashMap<Broker, Portfolio>,
    pub total_value: Decimal,
    pub total_unrealized_pnl: Decimal,
    pub total_realized_pnl: Decimal,
    pub reporting_currency: Currency,
}

impl ConsolidatedPortfolio {
    pub fn new() -> Self {
        Self {
            portfolios: HashMap::new(),
            total_value: Decimal::ZERO,
            total_unrealized_pnl: Decimal::ZERO,
            total_realized_pnl: Decimal::ZERO,
            reporting_currency: Currency::USD, // Default, will be overwritten
        }
    }

    pub fn from(ps: HashMap<Broker, Portfolio>) -> Self {
        Self {
            portfolios: ps,
            total_value: Decimal::ZERO,
            total_unrealized_pnl: Decimal::ZERO,
            total_realized_pnl: Decimal::ZERO,
            reporting_currency: Currency::USD, // Default, will be overwritten
        }
    }

    pub fn calculate_totals(
        &mut self,
        reporting_currency: Currency,
        prices: &HashMap<String, Vec<StockPrice>>,
    ) {
        let mut total_value = Decimal::ZERO;
        let mut total_unrealized_pnl = Decimal::ZERO;
        let mut total_realized_pnl = Decimal::ZERO;

        for (broker, portfolio) in self.portfolios.iter() {
            if let Some((_date, statement)) = portfolio.daily_statements.last_key_value() {
                let broker_reporting_currency = broker.reporting_currency();

                let conversion_rate = YFinanceSource::get_conversion_rate(
                    broker_reporting_currency,
                    reporting_currency,
                    prices,
                );

                total_value += statement.total_value * conversion_rate;
                total_unrealized_pnl += statement.total_unrealized_pnl_value * conversion_rate;
                total_realized_pnl += statement.total_realized_pnl_value * conversion_rate;
            }
        }

        self.total_value = total_value;
        self.total_unrealized_pnl = total_unrealized_pnl;
        self.total_realized_pnl = total_realized_pnl;
        self.reporting_currency = reporting_currency;
    }

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

    fn deref(&self) -> &Self::Target { &self.portfolios }
}

impl DerefMut for ConsolidatedPortfolio {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.portfolios }
}

pub struct ConsolidatedPortfolioDisplay<'a> {
    pub consolidated_portfolio: &'a ConsolidatedPortfolio,
    pub sort_by: SortBy,
    pub order: Order,
}

impl fmt::Display for ConsolidatedPortfolioDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let consolidated_portfolio = self.consolidated_portfolio;
        let reporting_currency = consolidated_portfolio.reporting_currency;

        // Display Consolidated Summary
        writeln!(f, "======================================================================================================================================================================================================")?;
        writeln!(f, "--- Consolidated Portfolio Summary (in {}) ---", reporting_currency)?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Portfolio Value", consolidated_portfolio.total_value, reporting_currency
        )?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Unrealized P&L", consolidated_portfolio.total_unrealized_pnl, reporting_currency
        )?;
        writeln!(
            f,
            "{:<25}: {:>10.2} {}",
            "Total Realized P&L", consolidated_portfolio.total_realized_pnl, reporting_currency
        )?;
        writeln!(f, "")?;

        for (broker, portfolio) in self.consolidated_portfolio.portfolios.iter() {
            writeln!(
                f,
                "\n--- Portfolio for Broker: {} ---",
                broker
            )?;
            let display = PortfolioDisplay {
                portfolio,
                sort_by: self.sort_by,
                order: self.order,
                reporting_currency: broker.reporting_currency(),
            };
            write!(f, "{}", display)?;
        }
        Ok(())
    }
}
