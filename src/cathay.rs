use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::portfolio::portfolio::{
    AssetClass, Broker, Currency, Portfolio, Security, Transaction, TransactionKind,
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

#[derive(Debug, Deserialize)]
pub struct CathayTradeRecord {
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

fn get_symbol_map() -> HashMap<&'static str, &'static str> {
    let mut map = HashMap::new();
    map.insert("元大台灣50", "0050.TW");
    map.insert("國泰永續高股息", "00878.TW");
    map.insert("元大美債20年", "00679B.TW");
    map.insert("元大投資級公司債", "00720B.TW");
    map.insert("中信關鍵半導體", "00891.TW");
    map.insert("星宇航空", "2646.TW");
    map.insert("大華優利高填息30", "00918.TW");
    map.insert("正崴", "2392.TW");
    map.insert("芯測", "6786.TWO");
    map.insert("台積電", "2330.TW");
    map
}

pub fn load_from_cathay_csv(portfolio: &mut Portfolio, file_path: &str) -> Result<()> {
    let symbol_map = get_symbol_map();
    let file = File::open(file_path)?;
    let mut reader = BufReader::new(file);

    // Skip the first line which is a disclaimer
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;

    let mut csv_reader =
        csv::ReaderBuilder::new().has_headers(true).flexible(true).from_reader(reader);

    for result in csv_reader.deserialize() {
        let record: CathayTradeRecord =
            result.context("Failed to deserialize Cathay trade record")?;
        let symbol = symbol_map.get(record.name.as_str()).ok_or_else(|| {
            anyhow::anyhow!("Symbol not found for Cathay stock name: {}", record.name)
        })?;

        let transaction: Transaction = (record, symbol.to_string()).into();

        let date = transaction.datetime.date_naive();
        portfolio
            .transactions
            .entry(date)
            .and_modify(|ts| ts.push(transaction.clone()))
            .or_insert_with(|| vec![transaction.clone()]);

        if !transaction.symbol.is_empty() {
            portfolio.securities.entry(transaction.symbol.clone()).or_insert_with(|| Security {
                symbol: transaction.symbol.clone(),
                description: "".to_string(), // Description can be added later if available
            });
        }
    }

    Ok(())
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

        Transaction {
            id: String::new(), // Will be generated later
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
        }
    }
}
