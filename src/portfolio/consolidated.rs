use std::{
    collections::HashMap,
    fmt,
    ops::{Deref, DerefMut},
};

use crate::{
    portfolio::portfolio::{Broker, Portfolio, PortfolioDisplay},
    prices::StockPrice,
    Order, SortBy,
};

#[derive(Debug, Default)]
pub struct ConsolidatedPortfolio {
    pub portfolios: HashMap<Broker, Portfolio>,
}

impl ConsolidatedPortfolio {
    pub fn new() -> Self { ConsolidatedPortfolio { portfolios: HashMap::new() } }
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
    pub prices: &'a HashMap<String, Vec<StockPrice>>,
}

impl fmt::Display for ConsolidatedPortfolioDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
                reporting_currency: portfolio.reporting_currency,
            };
            write!(f, "{}", display)?;
        }
        Ok(())
    }
}
