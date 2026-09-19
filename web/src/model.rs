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
    pub children: Vec<Node>,
}

/// 資產 or 負債.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub label: String,
    pub amounts: Vec<Money>,
    pub converted: Converted,
    pub nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceSheet {
    pub as_of: String,
    pub base: String,
    pub sections: Vec<Section>,
    pub net_worth: Converted,
}
