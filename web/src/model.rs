//! What the server sends a page: display-ready, so the wasm client needs no
//! ledger types.

use serde::{Deserialize, Serialize};

/// A formatted amount with its currency, e.g. `1,234.5 USD`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Money {
    pub text: String,
    pub negative: bool,
}

/// A sum converted into the base currency. Currencies with no rate are left
/// out and named, never converted at 1:1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Converted {
    pub money: Money,
    pub unpriced: Vec<String>,
}

/// One account in the tree, summing its own postings and its descendants'.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Identifies the node (fold state); never shown.
    pub path: String,
    pub label: String,
    /// One per currency held, nonzero only.
    pub amounts: Vec<Money>,
    /// Only when a currency other than the base is held.
    pub converted: Option<Converted>,
    /// A cost, not a valuation: shown, but left out of every total above it.
    pub at_cost: bool,
    pub children: Vec<Node>,
}

/// 資產 or 負債.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// The root path, identifying the section's fold state; never shown.
    pub path: String,
    pub label: String,
    pub amounts: Vec<Money>,
    /// Only when a currency other than the base is held, as on a row.
    pub converted: Option<Converted>,
    /// The section's own total in the base currency, always present — the
    /// summary shows one figure per section.
    pub total: Converted,
    /// The at-cost holdings this section's total leaves out.
    pub excluded: Option<Converted>,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceSheet {
    pub as_of: String,
    pub base: String,
    pub sections: Vec<Section>,
    pub net_worth: Converted,
    /// The at-cost holdings net worth leaves out.
    pub excluded: Option<Converted>,
}
