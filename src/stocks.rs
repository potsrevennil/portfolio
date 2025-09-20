use std::collections::HashMap;

use chrono::{DateTime, NaiveDate};
use rust_decimal::Decimal;

// --- Enums for Type-Safety ---

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuySell {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TransactionKind {
    Trade,
    Dividend,
    Interest,
    Fee,
    TransferIn,
    TransferOut,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Currency {
    USD,
    TWD,
}

// --- Refined Core Structs ---

// Security struct remains largely the same
pub struct Security {
    pub symbol: String,
    pub description: String,
    // ... other fields
}

pub struct Transaction {
    pub symbol: String,
    pub kind: TransactionKind,
    pub datetime: DateTime<chrono::Utc>, // Precise timestamp
    pub settle_date: NaiveDate,          // Just the date is enough
    pub buy_sell: Option<BuySell>,       // Not all transactions are buys/sells
    pub quantity: Decimal,               // Use Decimal for precision
    pub price: Option<Decimal>,          // Price might not apply to all kinds
    pub amount: Decimal,
    pub commission: Decimal,
    pub currency: Currency,
}

// --- New Struct for Calculated Holdings ---

/// Represents the current holding of a specific security.
#[derive(Debug)]
pub struct Holding {
    pub symbol: String,
    pub quantity: Decimal,
    pub average_cost_basis: Decimal, /* (Total cost) / (Total shares)
                                      * We could add more fields like current market value,
                                      * unrealized P/L, etc. */
}

// --- Refined Top-Level Struct ---

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

