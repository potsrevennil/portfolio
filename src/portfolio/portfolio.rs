use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use anyhow::Result;
use chrono::{DateTime, NaiveDate};
use clap::ValueEnum;
use csv;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{
    portfolio::{
        holding::{adjust_holdings, mark_to_market, settle_transactions, Holding},
        statement::Statement,
    },
    prices::StockPrice,
};

// --- Enums for Type-Safety ---

#[derive(Debug, Default, Clone)]
pub struct Event {
    pub transactions: Vec<Transaction>,
    pub splits: Vec<(String, Decimal)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
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

impl std::fmt::Display for TransactionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{:?}", self) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum AssetClass {
    Stocks,
    Cash,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    #[default]
    USD,
    TWD,
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Currency::USD => write!(f, "USD"),
            Currency::TWD => write!(f, "TWD"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum Broker {
    InteractiveBrokers,
    Cathay,
}

impl std::fmt::Display for Broker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Broker::InteractiveBrokers => write!(f, "InteractiveBrokers"),
            Broker::Cathay => write!(f, "Cathay"),
        }
    }
}

impl Broker {
    pub fn reporting_currency(&self) -> Currency {
        match self {
            Broker::Cathay => Currency::TWD,
            Broker::InteractiveBrokers => Currency::USD,
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

    pub fn to_csv_file(&self, file_path: &str) -> Result<()> {
        let mut writer = csv::Writer::from_path(file_path)?;
        for event in self.events.values() {
            // Sort transactions within the same day by datetime for consistent output
            let mut transactions = event.transactions.clone();
            transactions.sort_by_key(|t| t.datetime);

            for t in &transactions {
                let description =
                    self.securities.get(&t.symbol).map_or(String::new(), |s| s.description.clone());
                writer.serialize(CsvTransactionRecord {
                    id: t.id.clone(),
                    source: t.source,
                    asset_class: t.asset_class,
                    symbol: t.symbol.clone(),
                    description,
                    kind: t.kind,
                    datetime: t.datetime,
                    settle_date: t.settle_date,
                    quantity: t.quantity,
                    price: t.price,
                    amount: t.amount,
                    commission: t.commission,
                    currency: t.currency,
                    balance: t.balance,
                })?;
            }
        }
        writer.flush()?;
        Ok(())
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
