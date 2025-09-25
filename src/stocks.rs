use std::collections::{BTreeMap, HashMap};

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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{:?}", self) }
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
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Holding {
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
    pub transactions: BTreeMap<NaiveDate, Vec<Transaction>>,

    // The calculated current state of all holdings.
    // This is derived from the transaction history.

    // Daily snapshots of holdings and cash balances over a calculated range.
    pub daily_snapshots: HashMap<NaiveDate, (HashMap<String, Holding>, HashMap<Currency, Decimal>)>,
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
            daily_snapshots: HashMap::new(),
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
    pub fn calculate_holdings(
        &mut self,
        start_date: NaiveDate,
        end_date: NaiveDate,
        prices: &HashMap<String, Vec<StockPrice>>,
    ) {
        self.daily_snapshots.clear();

        let mut holdings: HashMap<String, Holding> = HashMap::new();
        let mut cash_balances: HashMap<Currency, Decimal> = HashMap::new();

        // Phase 1: Apply all transactions before start_date to initialize holdings/cash
        for (_d, ts) in self.transactions.iter().take_while(|&(&d, _)| d < start_date) {
            apply_daily_transactions(&mut holdings, &mut cash_balances, ts);
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
            apply_daily_transactions(&mut holdings, &mut cash_balances, ts);

            // Get prices for the current day
            let current_prices: HashMap<String, &StockPrice> = prices
                .iter()
                .filter_map(|(s, daily_prices)| {
                    let price_idx = daily_prices.partition_point(|p| p.date <= *d);
                    (price_idx > 0).then(|| (s.clone(), &daily_prices[price_idx - 1]))
                })
                .collect();

            // Calculate P&L for the current day's snapshot
            let daily_snapshot = calculate_daily_pnl(&mut holdings, &current_prices);

            self.daily_snapshots
                .entry(*d)
                .and_modify(|(h, c)| {
                    *h = daily_snapshot.clone();
                    *c = cash_balances.clone();
                })
                .or_insert((daily_snapshot, cash_balances.clone()));
        }

        // Handle the case where start_date has no transactions but needs a snapshot
        // This ensures a baseline snapshot for start_date if no transactions occurred
        // on it
        if self.daily_snapshots.get(&start_date).is_none() && start_date <= end_date {
            let current_prices: HashMap<String, &StockPrice> = prices
                .iter()
                .filter_map(|(s, daily_prices)| {
                    let price_idx = daily_prices.partition_point(|p| p.date <= start_date);
                    (price_idx > 0).then(|| (s.clone(), &daily_prices[price_idx - 1]))
                })
                .collect();
            let daily_snapshot = calculate_daily_pnl(&mut holdings, &current_prices);
            self.daily_snapshots
                .entry(start_date)
                .or_insert((daily_snapshot, cash_balances.clone()));
        }
    }
}

fn apply_daily_transactions(
    holdings: &mut HashMap<String, Holding>,
    cash_balances: &mut HashMap<Currency, Decimal>,
    ts: &[Transaction],
) {
    for t in ts {
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
            // Update cash balance first
            let cash_balance = cash_balances.entry(t.currency).or_insert(Decimal::ZERO);
            *cash_balance += t.amount + t.commission;
        }

        // Update holdings for stock-related transactions
        if t.asset_class == AssetClass::Stocks {
            let h = holdings.entry(t.symbol.clone()).or_insert(Holding::default());
            match t.kind {
                TransactionKind::Buy => {
                    h.quantity += t.quantity;
                    h.total_cost += t.quantity * t.price;
                }
                TransactionKind::Sell => {
                    if h.quantity > Decimal::ZERO {
                        let cost_basis_per_share =
                            h.total_cost.checked_div(h.quantity).unwrap_or_default();
                        let cost_of_sold_shares = t.quantity * cost_basis_per_share;
                        let proceeds = t.quantity * t.price;
                        let pnl = proceeds - cost_of_sold_shares;

                        h.realized_pnl_value += pnl;
                        // realized_pnl_percentage will be calculated after all transactions for the
                        // day
                        h.quantity -= t.quantity;
                        h.total_cost -= cost_of_sold_shares;
                    }
                }
                TransactionKind::Deposit => {
                    h.quantity += t.quantity;
                    // For deposits (e.g., from transfers), the cost is the market value at the time
                    h.total_cost += t.amount;
                }
                TransactionKind::Withdrawal => {
                    if h.quantity > Decimal::ZERO {
                        let cost_basis_per_share =
                            h.total_cost.checked_div(h.quantity).unwrap_or_default();
                        let cost_of_withdrawn_shares = t.quantity * cost_basis_per_share;
                        h.quantity -= t.quantity;
                        h.total_cost -= cost_of_withdrawn_shares;
                    }
                }
                TransactionKind::CorporateAction => {
                    h.quantity += t.quantity;
                }
                _ => {}
            }
        }
    }

    // Calculate realized_pnl_percentage and average_cost once for each stock after
    // all daily transactions
    for h in holdings.values_mut() {
        h.realized_pnl_percentage =
            h.realized_pnl_value.checked_div(h.total_cost).unwrap_or_default() * Decimal::from(100);
        h.average_cost = h.total_cost.checked_div(h.quantity).unwrap_or_default();
    }
}

fn calculate_daily_pnl(
    holdings: &mut HashMap<String, Holding>,
    prices: &HashMap<String, &StockPrice>,
) -> HashMap<String, Holding> {
    for (symbol, h) in holdings.iter_mut() {
        let market_price = prices
            .get(symbol)
            .map_or(Decimal::ZERO, |p| Decimal::from_f64(p.close_price).unwrap_or_default());
        h.market_value = h.quantity * market_price;
        h.unrealized_pnl_value = h.market_value - h.total_cost;
        h.unrealized_pnl_percentage =
            h.unrealized_pnl_value.checked_div(h.total_cost).unwrap_or_default()
                * Decimal::from(100);
    }

    // Snapshot all holdings, including zero-quantity ones, for realized PnL
    // tracking
    let snapshot = holdings.clone();
    holdings.retain(|_k, h| !h.quantity.is_zero());

    snapshot
}
