use std::{collections::BTreeMap, fmt};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumIter, EnumString};

use crate::portfolio::Broker;

/// What a statement line is. Every line is kept as its own record, so a
/// withholding and its refund are two records, never one netted figure.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString, EnumIter,
)]
#[strum(serialize_all = "kebab-case")]
pub enum RecordKind {
    Buy,
    Sell,
    Dividend,
    /// Tax withheld, or (positive) a withholding refunded.
    Withholding,
    Interest,
    Fee,
    Deposit,
    Withdrawal,
    TransferIn,
    TransferOut,
    /// The shares a split added (or removed).
    Split,
    AwardGrant,
    /// Informational: the shares were already held from the grant, so vesting
    /// moves nothing.
    AwardVesting,
    /// Shares withheld at vesting for tax.
    AwardWithholding,
    /// A move between the account's own sub-accounts (cash↔margin, lending
    /// types); each leg is kept, and the legs net to zero.
    Internal,
    /// What the account held before its first imported statement.
    Opening,
}

/// What a balance is counted in: a currency, or shares of one security.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Commodity {
    Cash(Currency),
    Security(String),
}

impl fmt::Display for Commodity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Commodity::Cash(c) => write!(f, "{c}"),
            Commodity::Security(s) => write!(f, "{s}"),
        }
    }
}

/// One line of a broker statement. `quantity` is the signed change in shares
/// of `symbol`; `amount + commission` the signed change in cash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokerRecord {
    pub kind: RecordKind,
    /// When the order executed, where the source says.
    pub trade_date: Option<NaiveDate>,
    /// The day the line moves the statement's balances.
    pub settle_date: NaiveDate,
    pub executed_at: Option<DateTime<Utc>>,
    pub symbol: Option<String>,
    pub quantity: Decimal,
    pub price: Decimal,
    pub amount: Decimal,
    pub commission: Decimal,
    pub currency: Currency,
    pub description: String,
    /// Unique per broker account; stable across overlapping downloads.
    pub key: String,
}

impl BrokerRecord {
    pub fn cash(&self) -> Decimal { self.amount + self.commission }
}

/// A statement's own figures at the end of one day. `positions` lists every
/// holding, so a security absent from it is held at zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holdings {
    pub cash: BTreeMap<Currency, Decimal>,
    pub positions: BTreeMap<String, Decimal>,
}

impl Holdings {
    pub fn is_empty(&self) -> bool {
        self.cash.values().all(Decimal::is_zero) && self.positions.values().all(Decimal::is_zero)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Commodity, Decimal)> + '_ {
        let cash = self.cash.iter().map(|(c, q)| (Commodity::Cash(*c), *q));
        let positions = self.positions.iter().map(|(s, q)| (Commodity::Security(s.clone()), *q));
        cash.chain(positions)
    }
}

/// One parsed statement file.
#[derive(Debug, Clone)]
pub struct BrokerStatement {
    pub broker: Broker,
    /// The broker's account number; empty when the export states none.
    pub account: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub generated: Option<NaiveDateTime>,
    /// Balances before `period_start`, where the statement states them in
    /// full.
    pub opening: Option<Holdings>,
    /// Balances at the end of `period_end`; `None` for exports without any.
    pub closing: Option<Holdings>,
    pub records: Vec<BrokerRecord>,
}

impl BrokerStatement {
    /// The day `opening` describes.
    pub fn opening_date(&self) -> NaiveDate {
        self.period_start.pred_opt().expect("a day before the period")
    }

    /// `opening` as records, for an account's first statement: what it held
    /// before anything imported explains.
    pub fn opening_records(&self) -> Vec<BrokerRecord> {
        let Some(opening) = &self.opening else { return Vec::new() };
        let date = self.opening_date();
        let base = BrokerRecord {
            kind: RecordKind::Opening,
            trade_date: None,
            settle_date: date,
            executed_at: None,
            symbol: None,
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount: Decimal::ZERO,
            commission: Decimal::ZERO,
            currency: self.broker.reporting_currency(),
            description: "opening balance".to_string(),
            key: String::new(),
        };
        opening
            .iter()
            .filter(|(_, q)| !q.is_zero())
            .map(|(commodity, q)| match commodity {
                Commodity::Cash(c) => BrokerRecord {
                    currency: c,
                    amount: q,
                    key: format!("opening:{c}"),
                    ..base.clone()
                },
                Commodity::Security(s) => BrokerRecord {
                    key: format!("opening:{s}"),
                    symbol: Some(s),
                    quantity: q,
                    ..base.clone()
                },
            })
            .collect()
    }
}

/// Adds one record's cash and share movements to a running balance.
pub fn accumulate(balances: &mut BTreeMap<Commodity, Decimal>, r: &BrokerRecord) {
    if !r.cash().is_zero() {
        *balances.entry(Commodity::Cash(r.currency)).or_default() += r.cash();
    }
    if let (Some(symbol), false) = (&r.symbol, r.quantity.is_zero()) {
        *balances.entry(Commodity::Security(symbol.clone())).or_default() += r.quantity;
    }
}

/// Balances after replaying `records` through the end of `as_of`, by settle
/// date. Where several days are asked for, walk the records once with
/// [`accumulate`] instead.
pub fn replay<'a>(
    records: impl IntoIterator<Item = &'a BrokerRecord>,
    as_of: NaiveDate,
) -> BTreeMap<Commodity, Decimal> {
    let mut balances = BTreeMap::new();
    for r in records.into_iter().filter(|r| r.settle_date <= as_of) {
        accumulate(&mut balances, r);
    }
    balances
}

/// Numbers repeats of an identical key within one source, `#2`, `#3`, …, so a
/// line that genuinely occurs twice stays two records. Stable as long as a
/// whole day is in one file, in the broker's order.
pub fn number_repeats(records: &mut [BrokerRecord]) {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for r in records {
        let n = seen.entry(r.key.clone()).or_default();
        *n += 1;
        if *n > 1 {
            r.key = format!("{}#{n}", r.key);
        }
    }
}
