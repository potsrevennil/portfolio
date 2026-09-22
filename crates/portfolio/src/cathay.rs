use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{BufRead, BufReader},
};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::portfolio::portfolio::{
    AssetClass, Broker, Currency, Event, Security, Transaction, TransactionKind,
};

// Custom deserialization utilities
mod de_utils {
    use chrono::NaiveDate;
    use rust_decimal::Decimal;
    use serde::{self, de::Error, Deserialize, Deserializer};

    // YYYY/MM/DD
    pub fn date_format<'de, D>(deserializer: D) -> Result<NaiveDate, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveDate::parse_from_str(&s, "%Y/%m/%d").map_err(D::Error::custom)
    }

    // Numbers with commas, possibly quoted
    pub fn decimal_from_string<'de, D>(deserializer: D) -> Result<Decimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.trim().is_empty() {
            Ok(Decimal::ZERO)
        } else {
            s.replace(",", "").parse::<Decimal>().map_err(D::Error::custom)
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct CathayTradeRecord {
    #[serde(rename = "委託書號")]
    pub id: String,
    #[serde(rename = "股名")]
    pub name: String,
    #[serde(rename = "日期", deserialize_with = "de_utils::date_format")]
    pub date: NaiveDate,
    #[serde(rename = "成交股數", deserialize_with = "de_utils::decimal_from_string")]
    pub quantity: Decimal,
    #[serde(rename = "淨收付金額", deserialize_with = "de_utils::decimal_from_string")]
    pub net_amount: Decimal,
    #[serde(rename = "買賣別")]
    pub kind: String,
    #[serde(rename = "成交價", deserialize_with = "de_utils::decimal_from_string")]
    pub price: Decimal,
    #[serde(rename = "成本", deserialize_with = "de_utils::decimal_from_string")]
    pub cost: Decimal,
    #[serde(rename = "手續費", deserialize_with = "de_utils::decimal_from_string")]
    pub commission: Decimal,
    #[serde(rename = "交易稅", deserialize_with = "de_utils::decimal_from_string")]
    pub tax: Decimal,
}

/// `symbols` maps each 股名 in the export to its ticker; see
/// `securities::Securities::symbols`.
pub fn load_from_csv(
    file_path: &str,
    symbols: &HashMap<String, String>,
) -> Result<(BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> {
    let file = File::open(file_path)?;
    let mut reader = BufReader::new(file);

    // Skip the first line which is a disclaimer
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;

    let mut csv_reader =
        csv::ReaderBuilder::new().has_headers(true).flexible(true).from_reader(reader);

    let mut events: BTreeMap<NaiveDate, Event> = BTreeMap::new();
    let mut securities: HashMap<String, Security> = HashMap::new();

    for result in csv_reader.deserialize() {
        let record: CathayTradeRecord =
            result.context("Failed to deserialize Cathay trade record")?;
        let symbol = symbols.get(&record.name).ok_or_else(|| {
            anyhow::anyhow!(
                "Symbol not found for Cathay stock name: {}; add it under [symbols]",
                record.name
            )
        })?;

        let transaction: Transaction = (record.clone(), symbol.to_string()).into();

        events
            .entry(transaction.datetime.date_naive())
            .or_insert_with(Event::default)
            .transactions
            .push(transaction.clone());

        // Generate implicit deposit/withdrawal transactions for Cathay
        let cash_flow_transaction = match transaction.kind {
            TransactionKind::Buy => {
                let mut t = Transaction {
                    id: String::new(),
                    source: Broker::Cathay,
                    asset_class: AssetClass::Cash, // This is a cash movement
                    symbol: "CASH".to_string(),    // Use a generic symbol for cash
                    kind: TransactionKind::Deposit,
                    datetime: transaction.datetime,
                    settle_date: transaction.settle_date,
                    quantity: Decimal::ZERO,         // No quantity for cash
                    price: Decimal::ZERO,            // No price for cash
                    amount: record.net_amount.abs(), // Absolute value of net_amount
                    commission: Decimal::ZERO,       /* Commission already accounted for in
                                                      * net_amount */
                    currency: Currency::TWD,
                    balance: Decimal::ZERO, // Will be calculated later
                };
                t.generate_id();
                Some(t)
            }
            TransactionKind::Sell => {
                let mut t = Transaction {
                    id: String::new(),
                    source: Broker::Cathay,
                    asset_class: AssetClass::Cash, // This is a cash movement
                    symbol: "CASH".to_string(),    // Use a generic symbol for cash
                    kind: TransactionKind::Withdrawal,
                    datetime: transaction.datetime,
                    settle_date: transaction.settle_date,
                    quantity: Decimal::ZERO, // No quantity for cash
                    price: Decimal::ZERO,    // No price for cash
                    amount: -record.net_amount.abs(),
                    commission: Decimal::ZERO, // Commission already accounted for in net_amount
                    currency: Currency::TWD,
                    balance: Decimal::ZERO, // Will be calculated later
                };
                t.generate_id();
                Some(t)
            }
            _ => None, // Other transaction kinds don't generate implicit cash flow
        };

        if let Some(cash_tx) = cash_flow_transaction {
            events
                .entry(cash_tx.datetime.date_naive())
                .or_insert_with(Event::default)
                .transactions
                .push(cash_tx);
        }

        if !transaction.symbol.is_empty() {
            securities.entry(transaction.symbol.clone()).or_insert_with(|| Security {
                symbol: transaction.symbol.clone(),
                description: record.name.clone(), // Use record.name as description
            });
        }
    }

    Ok((events, securities))
}

impl From<(CathayTradeRecord, String)> for Transaction {
    fn from((record, symbol): (CathayTradeRecord, String)) -> Self {
        let (kind, sign) = match record.kind.as_str() {
            "現買" => (TransactionKind::Buy, -1),
            "現賣" => (TransactionKind::Sell, 1),
            _ => (TransactionKind::Other, 0),
        };

        // The `cost` field in the CSV is the gross amount of the trade.
        // We make it negative for buys and positive for sells.
        let amount = record.cost * Decimal::from(sign);

        // Commissions and taxes are always costs (negative).
        let total_commission = -(record.commission + record.tax);

        let mut transaction = Transaction {
            id: String::new(), // Always generate a new ID
            source: Broker::Cathay,
            asset_class: AssetClass::Stocks,
            symbol,
            kind,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.date),
            quantity: record.quantity,
            price: record.price,
            amount,
            commission: total_commission,
            currency: Currency::TWD,
            balance: Decimal::ZERO, // Will be calculated later
        };

        transaction.generate_id();
        transaction
    }
}
