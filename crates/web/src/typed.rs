//! What a reader typed into a form, told apart from a mistake in the words
//! they'd recognise.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

/// Grouping commas and spaces are only how the amount was typed.
pub(crate) fn amount(text: &str) -> Result<Decimal> {
    let clean: String = text.chars().filter(|c| *c != ',' && !c.is_whitespace()).collect();
    clean.parse().with_context(|| format!("金額毋著：{text}"))
}

pub(crate) fn currency(text: &str) -> Result<Currency> {
    text.trim().parse().with_context(|| format!("幣別毋著：{text}"))
}

pub(crate) fn date(text: &str) -> Result<NaiveDate> {
    text.trim().parse().with_context(|| format!("日期毋著：{text}"))
}
