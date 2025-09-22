use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, NaiveDate};
use csv;
use rust_decimal::{prelude::FromPrimitive, Decimal};
use serde::{Deserialize, Serialize};

use crate::StockPrice;

// --- Enums for Type-Safety ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum TransactionKind {
    Buy,
    Sell,
    Dividend,   // Dividend payment
    Interest,   // Interest received
    Fee,        // Broker fees, ADR fees, etc.
    Tax,        // Withholding tax
    Deposit,    // Cash deposit
    Withdrawal, // Cash withdrawal
    #[serde(other)]
    Other,
}

impl std::fmt::Display for TransactionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{:?}", self) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AssetClass {
    Stk,
    Cash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    USD,
    TWD,
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

// --- New Struct for Calculated Holdings ---

/// Represents the current holding of a specific security.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Holding {
    pub symbol: String,
    pub quantity: Decimal,
    pub total_cost: Decimal,
    pub average_cost: Decimal,
    pub market_value: Decimal, // Current value based on the latest available price
    pub unrealized_pnl_value: Decimal, // Unrealized profit/loss in currency
    pub unrealized_pnl_percentage: Decimal, // Unrealized profit/loss as a percentage
    pub realized_pnl_value: Decimal, // Overall realized profit/loss in currency
    pub realized_pnl_percentage: Decimal, // Overall realized profit/loss as a percentage
}

// --- Refined Top-Level Struct ---

#[derive(Debug)]
pub struct Portfolio {
    // Static data about all securities ever transacted.
    pub securities: HashMap<String, Security>,

    // The raw, immutable log of all historical transactions.
    pub transactions: Vec<Transaction>,

    // The calculated current state of all holdings.
    // This is derived from the transaction history.
    pub holdings: HashMap<String, Holding>,

    // Current cash balances for each currency.
    pub cash_balances: HashMap<Currency, Decimal>,
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
    pub fn new() -> Self {
        Portfolio {
            securities: HashMap::new(),
            transactions: Vec::new(),
            holdings: HashMap::new(),
            cash_balances: HashMap::new(),
        }
    }

    pub fn load_from_csv(&mut self, file_path: &str) -> Result<()> {
        let mut reader = csv::Reader::from_path(file_path)?;
        let mut transaction_ids = std::collections::HashSet::new();
        for result in reader.deserialize() {
            let record: CsvTransactionRecord = result?;
            if !transaction_ids.insert(record.id.clone()) {
                eprintln!(
                    "Warning: Duplicate transaction ID found, skipping record: {}",
                    record.id
                );
                continue;
            }
            self.transactions.push(Transaction {
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
            });
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

    /// Calculates the current holdings, including market value, unrealized P&L,
    /// and realized P&L.
    ///
    /// Note: Market value, unrealized P&L, and realized P&L are calculated
    /// based on the latest available prices for the current date.
    /// Calculation of these values over a time range or lot-specific
    /// analysis is left for future work.
    pub fn calculate_holdings(&mut self, prices: &HashMap<String, Vec<StockPrice>>) {
        self.cash_balances.clear();
        let mut temp_holdings: HashMap<String, (Decimal, Decimal, Decimal, Decimal)> =
            HashMap::new(); // symbol -> (quantity, total_cost, average_cost, realized_pnl_value)

        // Sort transactions by datetime to ensure chronological processing
        self.transactions.sort_by_key(|t| t.datetime);

        // First pass: Calculate total_cost, quantity, average_cost, and realized P&L
        for t in &self.transactions {
            // Update cash balance
            let should_update_cash = match t.kind {
                TransactionKind::Buy
                | TransactionKind::Sell
                | TransactionKind::Dividend
                | TransactionKind::Interest
                | TransactionKind::Fee
                | TransactionKind::Tax => true,
                TransactionKind::Deposit | TransactionKind::Withdrawal => {
                    t.asset_class == AssetClass::Cash
                }
                _ => false,
            };

            if should_update_cash {
                *self.cash_balances.entry(t.currency).or_default() += t.amount;
            }

            let (quantity, total_cost, avg, realized_pnl_value) =
                temp_holdings.entry(t.symbol.clone()).or_insert((
                    Decimal::ZERO,
                    Decimal::ZERO,
                    Decimal::ZERO,
                    Decimal::ZERO, // Initialize realized_pnl_value
                ));

            match t.kind {
                TransactionKind::Buy => {
                    *quantity += t.quantity;
                    *total_cost += t.amount.abs();
                    *avg = total_cost.checked_div(*quantity).unwrap_or_default();
                }
                TransactionKind::Sell => {
                    let sold_quantity = t.quantity.abs();
                    let cost_of_sold_shares = *avg * sold_quantity;
                    let profit_loss = t.amount.abs() - cost_of_sold_shares;

                    *realized_pnl_value += profit_loss; // Update realized_pnl_value directly

                    *total_cost -= cost_of_sold_shares;
                    *quantity -= sold_quantity;
                }
                TransactionKind::Deposit if t.quantity != Decimal::ZERO => {
                    *quantity += t.quantity;
                    *total_cost += t.amount;
                    *avg = total_cost.checked_div(*quantity).unwrap_or_default();
                }
                _ => {}
            }
        }

        // Second pass: Create Holding structs with market value and P&L
        self.holdings = temp_holdings
            .into_iter()
            .filter(|(_, (quantity, ..))| !quantity.is_zero())
            .map(|(symbol, (quantity, total_cost, average_cost, security_realized_pnl_value))| { // Destructure realized_pnl_value
                let latest_price = prices
                    .get(&symbol)
                    .and_then(|p| p.last()) // Get the latest price (assuming sorted by date)
                    .map_or(Decimal::ZERO, |p| Decimal::from_f64(p.close_price).unwrap_or_default());

                let market_value = quantity * latest_price;
                let unrealized_pnl_value = market_value - total_cost;
                let unrealized_pnl_percentage = (unrealized_pnl_value.checked_div(total_cost).unwrap_or_default()) * Decimal::from(100);

                let security_realized_pnl_percentage = (security_realized_pnl_value.checked_div(total_cost).unwrap_or_default()) * Decimal::from(100);


                (
                    symbol.clone(),
                    Holding {
                        symbol,
                        quantity,
                        total_cost,
                        average_cost,
                        market_value,
                        unrealized_pnl_value,
                        unrealized_pnl_percentage,
                        realized_pnl_value: security_realized_pnl_value,
                        realized_pnl_percentage: security_realized_pnl_percentage,
                    },
                )
            })
            .collect();
    }

    pub fn to_csv_file(&self, file_path: &str) -> Result<()> {
        let mut writer = csv::Writer::from_path(file_path)?;
        for t in &self.transactions {
            let security = self
                .securities
                .get(&t.symbol)
                .cloned()
                .unwrap_or(Security { symbol: t.symbol.clone(), description: "".to_string() });

            writer.serialize(CsvTransactionRecord {
                id: t.id.clone(),
                source: t.source,
                asset_class: t.asset_class.clone(),
                symbol: t.symbol.clone(),
                description: security.description,
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
        writer.flush()?;
        Ok(())
    }
}
