use std::collections::HashMap;

use chrono::{DateTime, NaiveDate};
use csv;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

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
    pub source: Broker,
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
    pub average_cost: Decimal, /* (Total cost) / (Total shares)
                                * We could add more fields like current market value,
                                * unrealized P/L, etc. */
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
    pub source: Broker,
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

    pub fn load_from_csv(&mut self, file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut reader = csv::Reader::from_path(file_path)?;
        for result in reader.deserialize() {
            let record: CsvTransactionRecord = result?;
            self.transactions.push(Transaction {
                source: record.source,
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
            self.securities.entry(record.symbol.clone()).or_insert(Security {
                symbol: record.symbol.clone(),
                description: record.description,
            });
        }
        Ok(())
    }

    pub fn calculate_holdings(&mut self) {
        let mut temp_holdings: HashMap<String, (Decimal, Decimal, Decimal)> = HashMap::new(); // symbol -> (quantity, total_cost)

        for t in &self.transactions {
            let (quantity, total_cost, avg) = temp_holdings.entry(t.symbol.clone()).or_insert((
                Decimal::ZERO,
                Decimal::ZERO,
                Decimal::ZERO,
            ));
            match t.kind {
                TransactionKind::Buy => {
                    *quantity += t.quantity;
                    *total_cost += t.amount.abs();
                    *avg = total_cost.checked_div(*quantity).unwrap_or_default();
                }
                TransactionKind::Sell => {
                    let sold_quantity = t.quantity.abs();
                    *total_cost -= *avg * sold_quantity;
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

        self.holdings = temp_holdings
            .into_iter()
            .filter(|(_, (quantity, ..))| !quantity.is_zero())
            .map(|(symbol, (quantity, total_cost, average_cost))| {
                (symbol.clone(), Holding { symbol, quantity, total_cost, average_cost })
            })
            .collect();
    }

    pub fn to_csv_file(&self, file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut writer = csv::Writer::from_path(file_path)?;
        for t in &self.transactions {
            let security = self
                .securities
                .get(&t.symbol)
                .cloned()
                .unwrap_or(Security { symbol: t.symbol.clone(), description: "".to_string() });

            writer.serialize(CsvTransactionRecord {
                source: t.source,
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
