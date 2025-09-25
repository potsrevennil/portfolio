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
    portfolio::holding::{mark_to_market, settle_transactions, Holding, HoldingDisplay},
    prices::{PriceService, StockPrice},
};

// --- Enums for Type-Safety ---

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
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
    Firstrade,
}

impl std::fmt::Display for Broker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Broker::InteractiveBrokers => write!(f, "InteractiveBrokers"),
            Broker::Firstrade => write!(f, "Firstrade"),
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

    // The raw, immutable log of all historical transactions.
    pub transactions: BTreeMap<NaiveDate, Vec<Transaction>>,

    // The calculated current state of all holdings.
    // This is derived from the transaction history.

    // Daily snapshots of holdings and cash balances over a calculated range.
    pub daily_statements:
        BTreeMap<NaiveDate, (HashMap<String, Holding>, HashMap<Currency, Decimal>)>,
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

impl Default for Portfolio {
    fn default() -> Self { Self::new() }
}

impl Portfolio {
    pub fn new() -> Self {
        Portfolio {
            securities: HashMap::new(),
            transactions: BTreeMap::new(),
            daily_statements: BTreeMap::new(),
        }
    }

    pub fn load_from_csv(&mut self, file_path: &str) -> Result<()> {
        let mut reader = csv::Reader::from_path(file_path)?;
        for result in reader.deserialize() {
            let record: CsvTransactionRecord = result?;
            let t = Transaction {
                id: record.id,
                source: record.source,
                asset_class: record.asset_class,
                symbol: record.symbol.clone(),
                kind: record.kind,
                datetime: record.datetime,
                settle_date: record.settle_date,
                quantity: record.quantity,
                price: record.price,
                amount: record.amount,
                commission: record.commission,
                currency: record.currency,
                balance: record.balance,
            };

            let date = t.datetime.date_naive();
            self.transactions.entry(date).and_modify(|ts| ts.push(t.clone())).or_insert(vec![t]);

            // Only insert into securities map if the symbol is not empty
            if !record.symbol.is_empty() {
                self.securities.entry(record.symbol.clone()).or_insert(Security {
                    symbol: record.symbol.clone(),
                    description: record.description,
                });
            }
        }
        Ok(())
    }

    pub fn to_csv_file(&self, file_path: &str) -> Result<()> {
        let mut writer = csv::Writer::from_path(file_path)?;
        for ts in self.transactions.values() {
            // Sort transactions within the same day by datetime for consistent output
            let mut ts = ts.clone();
            ts.sort_by_key(|t| t.datetime);

            for t in ts {
                let description =
                    self.securities.get(&t.symbol).map_or("", |s| &s.description).to_string();
                writer.serialize(CsvTransactionRecord {
                    id: t.id,
                    source: t.source,
                    asset_class: t.asset_class,
                    symbol: t.symbol,
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

        // Phase 1: Apply all transactions before start_date to initialize holdings/cash
        for (_d, ts) in self.transactions.iter().take_while(|&(&d, _)| d < start_date) {
            settle_transactions(&mut holdings, &mut cash_balances, ts);
        }

        // Phase 2: Iterate from start_date to end_date, processing transactions and
        // creating snapshots
        for (d, ts) in self
            .transactions
            .iter()
            .skip_while(|&(&d, _)| d < start_date)
            .take_while(|&(&d, _)| d >= start_date && d <= end_date)
        {
            // Apply transactions for the current day
            settle_transactions(&mut holdings, &mut cash_balances, ts);

            // Get prices for the current day
            let current_prices: HashMap<String, &StockPrice> = prices
                .iter()
                .filter_map(|(s, daily_prices)| {
                    let price_idx = daily_prices.partition_point(|p| p.date <= *d);
                    (price_idx > 0).then(|| (s.clone(), &daily_prices[price_idx - 1]))
                })
                .collect();

            // Calculate P&L for the current day's snapshot
            let daily_statement = mark_to_market(&mut holdings, &current_prices);

            self.daily_statements
                .entry(*d)
                .and_modify(|(h, c)| {
                    *h = daily_statement.clone();
                    *c = cash_balances.clone();
                })
                .or_insert((daily_statement, cash_balances.clone()));
        }

        // Handle the case where start_date has no transactions but needs a snapshot
        // This ensures a baseline snapshot for start_date if no transactions occurred
        // on it
        if self.daily_statements.get(&start_date).is_none() && start_date <= end_date {
            let current_prices: HashMap<String, &StockPrice> = prices
                .iter()
                .filter_map(|(s, daily_prices)| {
                    let price_idx = daily_prices.partition_point(|p| p.date <= start_date);
                    (price_idx > 0).then(|| (s.clone(), &daily_prices[price_idx - 1]))
                })
                .collect();
            let daily_statement = mark_to_market(&mut holdings, &current_prices);
            self.daily_statements
                .entry(start_date)
                .or_insert((daily_statement, cash_balances.clone()));
        }
    }

    pub async fn generate_report(
        &mut self,
        start_date: NaiveDate,
        end_date: NaiveDate,
        sort_by: SortBy,
        order: Order,
        price_service: &PriceService,
    ) -> Result<()> {
        // Fetch prices for all securities in the portfolio
        let unique_symbols: Vec<&str> = self.securities.keys().map(|s| s.as_str()).collect();
        let prices = price_service.get_prices(&unique_symbols, start_date, end_date).await?;

        self.generate_daily_statements(start_date, end_date, &prices);

        let portfolio_display =
            PortfolioDisplay { portfolio: self, sort_by, order, prices: &prices };
        println!("{}", portfolio_display);

        Ok(())
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
}

impl fmt::Display for PortfolioDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let portfolio = self.portfolio;
        let sort_by = self.sort_by;
        let order = self.order;
        let prices = self.prices;

        if portfolio.daily_statements.is_empty() {
            writeln!(f, "No portfolio data available for the selected period.")?;
            return Ok(());
        }

        for (date, (holdings, cash_balances)) in &portfolio.daily_statements {
            writeln!(f, "\n======================================================================================================================================================================================================")?;
            writeln!(f, "--- Account Summary as of {} ---", date)?;

            let total_holdings_market_value: Decimal =
                holdings.values().map(|h| h.market_value).sum();
            let total_cash: Decimal = cash_balances.values().sum();
            let total_assets_usd = total_holdings_market_value + total_cash;

            let total_unrealized_pnl: Decimal =
                holdings.values().map(|h| h.unrealized_pnl_value).sum();
            let total_realized_pnl: Decimal = holdings.values().map(|h| h.realized_pnl_value).sum();

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

            let mut holdings_vec: Vec<_> = holdings.iter().collect();
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
                width = name_col_width,
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
            for (currency, balance) in cash_balances {
                writeln!(f, "{:?}: {:.2}", currency, balance)?;
            }
        }
        Ok(())
    }
}
