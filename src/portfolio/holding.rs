use std::collections::HashMap;
use std::fmt; // Needed for fmt::Display

use rust_decimal::{prelude::FromPrimitive, Decimal};
use serde::{Deserialize, Serialize};

use crate::{
    portfolio::portfolio::{AssetClass, Currency, Security, Transaction, TransactionKind},
    prices::StockPrice,
};

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

pub fn settle_transactions(
    holdings: &mut HashMap<String, Holding>,
    cash_balances: &mut HashMap<Currency, Decimal>,
    transactions_for_day: &[Transaction],
) {
    for t in transactions_for_day {
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
                TransactionKind::Deposit => {
                    h.quantity += t.quantity;
                    // For deposits (e.g., from transfers), the cost is the market value at the time
                    h.total_cost += t.amount;
                }
                TransactionKind::Withdrawal => {
                    let cost_basis_per_share =
                        h.total_cost.checked_div(h.quantity).unwrap_or_default();
                    let cost_of_withdrawn_shares = t.quantity * cost_basis_per_share;
                    h.quantity -= t.quantity;
                    h.total_cost -= cost_of_withdrawn_shares;
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

pub fn mark_to_market(
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

pub fn adjust_holdings(holdings: &mut HashMap<String, Holding>, splits: &[(String, Decimal)]) {
    for (symbol, ratio) in splits {
        if let Some(holding) = holdings.get_mut(symbol) {
            holding.quantity *= ratio;
            holding.average_cost = holding.average_cost.checked_div(*ratio).unwrap_or_default();
        }
    }
}

pub struct HoldingDisplay<'a> {
    pub symbol: &'a String,
    pub holding: &'a Holding,
    pub securities: &'a HashMap<String, Security>,
    pub total_market_value: Decimal,
    pub prices: &'a HashMap<String, Vec<StockPrice>>,
    pub name_col_width: usize,
    pub reporting_currency: Currency,
}

impl fmt::Display for HoldingDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let percentage =
            self.holding.market_value.checked_div(self.total_market_value).unwrap_or_default()
                * Decimal::from(100);
        let name = self.securities.get(self.symbol).map_or("", |s| &s.description);
        let name_width = unicode_width::UnicodeWidthStr::width(name);
        let padding = self.name_col_width.saturating_sub(name_width);
        let name_part = format!("{}{}", name, " ".repeat(padding));

        let market_price = self.prices
            .get(self.symbol)
            .and_then(|p| p.last()) // Get the latest price (assuming sorted by date)
            .map_or(Decimal::ZERO, |p| Decimal::from_f64(p.close_price).unwrap_or_default());

        write!(
            f,
            "{:<15} {} {:>12.4} {:>18.2} {:>18.2} {:>15.2} {:>14.2}% {:>15.2} {:>14.2}% {:>13.2}% \
             {:>11.2}", // Added {:>11.2} for Market Price
            self.symbol,
            name_part,
            self.holding.quantity,
            self.holding.total_cost,
            self.holding.market_value,
            self.holding.unrealized_pnl_value,
            self.holding.unrealized_pnl_percentage,
            self.holding.realized_pnl_value,
            self.holding.realized_pnl_percentage,
            percentage,
            market_price, // Use direct market price
        )
    }
}
