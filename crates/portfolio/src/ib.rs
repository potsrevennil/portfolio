//! IB activity statements for the portfolio tracker, from the typed records
//! of [`crate::broker::ib`].

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::{
    broker::{self, BrokerRecord, RecordKind},
    portfolio::portfolio::{AssetClass, Broker, Event, Security, Transaction, TransactionKind},
};

pub fn load_from_csv(
    file_path: &str,
) -> Result<(BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> {
    let text = std::fs::read_to_string(file_path).with_context(|| file_path.to_string())?;
    let statement = broker::ib::parse(&text).with_context(|| file_path.to_string())?;

    let mut events: BTreeMap<NaiveDate, Event> = BTreeMap::new();
    let mut securities: HashMap<String, Security> = HashMap::new();
    for r in &statement.records {
        if let Some(symbol) = &r.symbol {
            securities
                .entry(symbol.clone())
                .or_insert_with(|| Security { symbol: symbol.clone(), description: String::new() });
        }
        let event = events.entry(r.settle_date).or_default();
        match (r.kind, split_ratio(r)) {
            (RecordKind::Split, Some(ratio)) => {
                event.splits.push((r.symbol.clone().unwrap_or_default(), ratio))
            }
            _ => event.transactions.extend(Option::<Transaction>::from(r)),
        }
    }
    events.retain(|_, e| !e.transactions.is_empty() || !e.splits.is_empty());
    Ok((events, securities))
}

/// The tracker's view of a record; `None` for lines that move nothing it
/// tracks (vesting, internal moves).
impl From<&BrokerRecord> for Option<Transaction> {
    fn from(r: &BrokerRecord) -> Self {
        let (kind, asset_class) = match r.kind {
            RecordKind::Buy => (TransactionKind::Buy, AssetClass::Stocks),
            RecordKind::Sell => (TransactionKind::Sell, AssetClass::Stocks),
            RecordKind::Dividend => (TransactionKind::Dividend, AssetClass::Cash),
            RecordKind::Withholding => (TransactionKind::Tax, AssetClass::Cash),
            RecordKind::Interest => (TransactionKind::Interest, AssetClass::Cash),
            RecordKind::Fee => (TransactionKind::Fee, AssetClass::Cash),
            RecordKind::Deposit => (TransactionKind::Deposit, AssetClass::Cash),
            RecordKind::Withdrawal => (TransactionKind::Withdrawal, AssetClass::Cash),
            RecordKind::TransferIn | RecordKind::AwardGrant | RecordKind::Opening => {
                (TransactionKind::Deposit, AssetClass::Stocks)
            }
            RecordKind::TransferOut | RecordKind::AwardWithholding => {
                (TransactionKind::Withdrawal, AssetClass::Stocks)
            }
            RecordKind::Split => (TransactionKind::CorporateAction, AssetClass::Stocks),
            RecordKind::AwardVesting | RecordKind::Internal => return None,
        };
        let quantity = r.quantity.abs();
        // A stock deposit carries its value in, as the tracker's cost basis.
        let amount = match (asset_class, kind) {
            (AssetClass::Stocks, TransactionKind::Deposit) => quantity * r.price,
            _ => r.amount,
        };
        Some(Transaction {
            id: format!("{:x}", gxhash::gxhash64(r.key.as_bytes(), 0)),
            source: Broker::InteractiveBrokers,
            asset_class,
            symbol: r.symbol.clone().unwrap_or_default(),
            kind,
            datetime: r.settle_date.and_time(chrono::NaiveTime::MIN).and_utc(),
            settle_date: Some(r.settle_date),
            quantity,
            price: r.price,
            amount,
            commission: r.commission,
            currency: r.currency,
            balance: Decimal::ZERO,
        })
    }
}

/// `… Split 4 for 1 …` → 4.
fn split_ratio(r: &BrokerRecord) -> Option<Decimal> {
    let (_, after) = r.description.split_once(" Split ")?;
    let mut words = after.split_whitespace();
    let to: Decimal = words.next()?.parse().ok()?;
    let _for = words.next()?;
    let from: Decimal = words.next()?.parse().ok()?;
    to.checked_div(from)
}
