//! What the importers need to know about your securities that the broker
//! exports do not say. See `securities.example.toml`.

use std::{collections::HashMap, fs};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::split::store::StockSplits;

#[derive(Debug, Default, Deserialize)]
pub struct Securities {
    /// Security name → Yahoo Finance ticker, for exports that name a security
    /// rather than giving its ticker.
    #[serde(default)]
    pub symbols: HashMap<String, String>,
    /// Splits no export reports, applied alongside the ones that are.
    #[serde(default)]
    pub splits: Vec<Split>,
}

#[derive(Debug, Deserialize)]
pub struct Split {
    pub symbol: String,
    pub date: NaiveDate,
    /// New shares per old share: 4 for a 4-for-1 split.
    pub ratio: Decimal,
}

impl Securities {
    /// Where the CLI looks, relative to the working directory, like
    /// `sqlite.db`.
    pub const PATH: &'static str = "securities.toml";

    pub fn load(path: &str) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading {} (copy securities.example.toml to start)", path))?;
        let securities: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path))?;
        securities.validate().with_context(|| format!("in {}", path))?;
        Ok(securities)
    }

    /// Fails on entries that would otherwise surface far from their cause: an
    /// empty ticker as a failed price lookup, a non-positive ratio as holdings
    /// that silently vanish or go negative.
    fn validate(&self) -> Result<()> {
        for (name, symbol) in &self.symbols {
            anyhow::ensure!(!name.trim().is_empty(), "symbols has an entry with an empty name");
            anyhow::ensure!(!symbol.trim().is_empty(), "symbols.{name} must not be empty");
        }
        for (i, split) in self.splits.iter().enumerate() {
            anyhow::ensure!(
                !split.symbol.trim().is_empty(),
                "splits[{i}].symbol must not be empty"
            );
            anyhow::ensure!(
                split.ratio > Decimal::ZERO,
                "splits[{i}].ratio must be positive, got {}",
                split.ratio
            );
        }
        Ok(())
    }

    /// The configured splits, keyed by date the way the split store expects.
    pub fn stock_splits(&self) -> StockSplits {
        let mut splits = StockSplits::new();
        for split in &self.splits {
            splits.entry(split.date).or_default().push((split.symbol.clone(), split.ratio));
        }
        splits
    }
}
