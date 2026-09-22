use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use chrono::{DateTime, NaiveDate};
use clap::ValueEnum;
pub use ledger_types::currency::Currency;
use prices::StockPrice;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumIter};

use crate::portfolio::{
    holding::{adjust_holdings, mark_to_market, settle_transactions, Holding},
    statement::Statement,
};

// --- Enums for Type-Safety ---

#[derive(Debug, Default, Clone)]
pub struct Event {
    pub transactions: Vec<Transaction>,
    pub splits: Vec<(String, Decimal)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Display, EnumIter)]
pub enum TransactionKind {
    Buy,
    Sell,
    Dividend,        // Dividend payment
    Interest,        // Interest received
    Fee,             // Broker fees, ADR fees, etc.
    Tax,             // Withholding tax
    Deposit,         // Cash deposit
    Withdrawal,      // Cash withdrawal
    CorporateAction, // Stock split or merge
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Display, EnumIter)]
pub enum AssetClass {
    Stocks,
    Cash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Display, EnumIter)]
pub enum Broker {
    InteractiveBrokers,
    Cathay,
    Pionex,
    Firstrade,
}

impl Broker {
    pub fn reporting_currency(&self) -> Currency {
        match self {
            Broker::Cathay => Currency::TWD,
            Broker::InteractiveBrokers => Currency::USD,
            Broker::Pionex => Currency::USD,
            Broker::Firstrade => Currency::USD,
        }
    }
}

// --- Refined Core Structs ---

// Security struct remains largely the same
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Security {
    pub symbol: String,
    pub description: String,
    // ... other fields
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Transaction {
    pub id: String,
    pub source: Broker,
    pub asset_class: AssetClass,
    pub symbol: String,
    pub kind: TransactionKind,
    pub datetime: DateTime<chrono::Utc>,
    pub settle_date: Option<NaiveDate>,
    pub quantity: Decimal,
    pub price: Decimal,
    pub amount: Decimal,
    pub commission: Decimal,
    pub currency: Currency,
    pub balance: Decimal,
}

impl Transaction {
    pub fn generate_id(&mut self) {
        let id_string = format!("{}-{}-{}", self.datetime, self.kind, self.symbol);
        self.id = format!("{:x}", gxhash::gxhash64(id_string.as_bytes(), 0));
    }
}

impl fmt::Display for Transaction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "--- Transaction Details ---")?;
        writeln!(f, "{:<15}: {}", "ID", self.id)?; // ID is a hash, maybe not always needed for display
        writeln!(f, "{:<15}: {}", "Source", self.source)?; // Broker
        writeln!(f, "{:<15}: {}", "Asset Class", self.asset_class)?;
        writeln!(f, "{:<15}: {}", "Symbol", self.symbol)?;
        writeln!(f, "{:<15}: {}", "Kind", self.kind)?;
        writeln!(f, "{:<15}: {}", "Date/Time", self.datetime.to_rfc2822())?;
        if let Some(settle_date) = self.settle_date {
            writeln!(f, "{:<15}: {}", "Settle Date", settle_date)?;
        }
        writeln!(f, "{:<15}: {}", "Quantity", self.quantity)?;
        writeln!(f, "{:<15}: {}", "Price", self.price)?;
        writeln!(f, "{:<15}: {}", "Amount", self.amount)?;
        writeln!(f, "{:<15}: {}", "Commission", self.commission)?;
        writeln!(f, "{:<15}: {}", "Currency", self.currency)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct Portfolio {
    // Static data about all securities ever transacted.
    pub securities: HashMap<String, Security>,
    pub reporting_currency: Currency,

    // The raw, immutable log of all historical events.
    pub events: BTreeMap<NaiveDate, Event>,

    // Daily snapshots of holdings and cash balances over a calculated range.
    pub daily_statements: BTreeMap<NaiveDate, Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsvTransactionRecord {
    pub id: String,
    pub source: Broker,
    pub asset_class: AssetClass,
    pub symbol: String,
    pub description: String,
    pub kind: TransactionKind,
    pub datetime: DateTime<chrono::Utc>,
    pub settle_date: Option<NaiveDate>,
    pub quantity: Decimal,
    pub price: Decimal,
    pub amount: Decimal,
    pub commission: Decimal,
    pub currency: Currency,
    pub balance: Decimal,
}

impl Portfolio {
    pub fn new(
        broker: Broker,
        events: BTreeMap<NaiveDate, Event>,
        securities: HashMap<String, Security>,
    ) -> Self {
        Portfolio {
            securities,
            events,
            daily_statements: BTreeMap::new(),
            reporting_currency: broker.reporting_currency(),
        }
    }

    /// Calculates the current holdings, including market value, unrealized P&L,
    /// and realized P&L.
    ///
    /// Note: Market value, unrealized P&L, and realized P&L are calculated
    /// based on the latest available prices for the current date.
    /// Calculation of these values over a time range or lot-specific
    /// analysis is left for future work.
    pub fn generate_daily_statements(
        &mut self,
        start_date: NaiveDate,
        end_date: NaiveDate,
        prices: &HashMap<String, Vec<StockPrice>>,
    ) {
        self.daily_statements.clear();

        let mut holdings: HashMap<String, Holding> = HashMap::new();
        let mut cash_balances: HashMap<Currency, Decimal> = HashMap::new();

        // Phase 1: Apply all events before start_date to initialize holdings/cash
        for (_d, event) in self.events.iter().take_while(|&(&d, _)| d < start_date) {
            adjust_holdings(&mut holdings, &event.splits);
            settle_transactions(&mut holdings, &mut cash_balances, &event.transactions);
        }

        // Phase 2: Iterate from start_date to end_date, processing events and
        // creating snapshots
        for (d, event) in self
            .events
            .iter()
            .skip_while(|&(&d, _)| d < start_date)
            .take_while(|&(&d, _)| d >= start_date && d <= end_date)
        {
            adjust_holdings(&mut holdings, &event.splits);
            settle_transactions(&mut holdings, &mut cash_balances, &event.transactions);

            let current_prices: HashMap<String, &StockPrice> = prices
                .iter()
                .filter_map(|(s, daily_prices)| {
                    let price_idx = daily_prices.partition_point(|p| p.date <= *d);
                    (price_idx > 0).then(|| (s.clone(), &daily_prices[price_idx - 1]))
                })
                .collect();

            let daily_holdings = mark_to_market(&mut holdings, &current_prices);

            let statement = Statement::from(
                daily_holdings,
                cash_balances.clone(),
                prices,
                self.reporting_currency,
            );
            self.daily_statements.insert(*d, statement);
        }

        if self.daily_statements.get(&start_date).is_none() && start_date <= end_date {
            let current_prices: HashMap<String, &StockPrice> = prices
                .iter()
                .filter_map(|(s, daily_prices)| {
                    let price_idx = daily_prices.partition_point(|p| p.date <= start_date);
                    (price_idx > 0).then(|| (s.clone(), &daily_prices[price_idx - 1]))
                })
                .collect();
            let daily_holdings = mark_to_market(&mut holdings, &current_prices);
            let statement = Statement::from(
                daily_holdings,
                cash_balances.clone(),
                prices,
                self.reporting_currency,
            );
            self.daily_statements.insert(start_date, statement);
        }
    }
}

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
    pub reporting_currency: Currency,
    pub total_consolidated_portfolio_value: Decimal,
    pub consolidated_reporting_currency: Currency,
}

use crate::portfolio::statement::StatementDisplay;

impl fmt::Display for PortfolioDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.portfolio.daily_statements.is_empty() {
            writeln!(f, "No portfolio data available for the selected period.")?;
            return Ok(());
        }

        for (date, statement) in &self.portfolio.daily_statements {
            let statement_display = StatementDisplay {
                date: *date,
                statement,
                securities: &self.portfolio.securities,
                prices: self.prices,
                sort_by: self.sort_by,
                order: self.order,
                reporting_currency: self.reporting_currency,
                total_consolidated_portfolio_value: self.total_consolidated_portfolio_value,
                consolidated_reporting_currency: self.consolidated_reporting_currency,
            };
            write!(f, "{}", statement_display)?;
        }
        Ok(())
    }
}
