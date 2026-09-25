//! Cathay securities trade history (國泰證券 app CSV).
//!
//! Trades only: the export states no balances or positions, and settlement
//! cash moves through the bank, so nothing here can be asserted.

use std::collections::HashMap;

use anyhow::{Context, Result};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;

use super::record::{number_repeats, BrokerRecord, BrokerStatement, RecordKind};
use crate::{cathay::CathayTradeRecord, portfolio::Broker};

pub const REF_PREFIX: &str = "cathay-securities:";

/// `names` maps each 股名 to its ticker (securities.toml `[symbols]`).
pub fn parse(text: &str, names: &HashMap<String, String>) -> Result<BrokerStatement> {
    // The first line is a banner, not the header.
    let body = text.trim_start_matches('\u{feff}').split_once('\n').map_or("", |(_, b)| b);
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(body.as_bytes());

    let mut records = Vec::new();
    for row in reader.deserialize() {
        let t: CathayTradeRecord = row.context("reading a Cathay trade row")?;
        let symbol = names.get(&t.name).with_context(|| {
            format!("no ticker for Cathay stock name {:?}; add it under [symbols]", t.name)
        })?;
        let (kind, sign) = match t.kind.as_str() {
            "現買" => (RecordKind::Buy, Decimal::ONE),
            "現賣" => (RecordKind::Sell, Decimal::NEGATIVE_ONE),
            other => anyhow::bail!("Cathay trade side {other:?}"),
        };
        let amount = -sign * t.cost;
        let commission = -(t.commission + t.tax);
        anyhow::ensure!(
            amount + commission == t.net_amount,
            "{} on {}: cost and fees don't add up to the net {}",
            t.name,
            t.date,
            t.net_amount
        );
        records.push(BrokerRecord {
            kind,
            trade_date: Some(t.date),
            settle_date: t.date,
            executed_at: None,
            symbol: Some(symbol.clone()),
            quantity: sign * t.quantity,
            price: t.price,
            amount,
            commission,
            currency: Currency::TWD,
            key: format!("{}:{}", t.date, t.id),
            description: t.name,
        });
    }

    // One order filled in several executions prints the same 委託書號 on each
    // row, so the key alone would collapse them into one trade.
    number_repeats(&mut records);

    let dates = records.iter().map(|r| r.settle_date);
    let (start, end) = (dates.clone().min(), dates.max());
    Ok(BrokerStatement {
        broker: Broker::Cathay,
        account: String::new(),
        period_start: start.context("an empty Cathay export")?,
        period_end: end.context("an empty Cathay export")?,
        generated: None,
        opening: None,
        closing: None,
        records,
    })
}
