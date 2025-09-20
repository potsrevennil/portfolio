use std::error::Error;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::stocks::{BuySell, Currency, Portfolio, Security, Transaction, TransactionKind};

mod de_utils {
    use chrono::{DateTime, Utc};
    use serde::{de, Deserialize, Deserializer};

    pub fn deserialize_datetime<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let parts: Vec<&str> = s.split(';').collect();
        if parts.len() != 2 {
            return Err(de::Error::custom("Invalid datetime format"));
        }
        let date_time_str = format!("{} {}", parts[0], parts[1]);
        let naive_dt = chrono::NaiveDateTime::parse_from_str(&date_time_str, "%Y-%m-%d %H%M%S")
            .map_err(de::Error::custom)?;
        Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive_dt, Utc))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbRecord {
    #[serde(rename = "CurrencyPrimary")]
    currency: Currency,
    symbol: String,
    description: String,
    transaction_type: TransactionKind,
    #[serde(rename = "Date/Time", deserialize_with = "de_utils::deserialize_datetime")]
    datetime: DateTime<Utc>,
    settle_date: NaiveDate,
    #[serde(rename = "Buy/Sell")]
    buy_sell: Option<BuySell>,
    quantity: Decimal,
    price: Option<Decimal>,
    amount: Decimal,
    commission: Decimal,
    broker_execution_commission: Decimal,
    broker_clearing_commission: Decimal,
    other_commission: Decimal,
    tax: Decimal,
}

pub fn load_from_ib_csv(portfolio: &mut Portfolio, file_path: &str) -> Result<(), Box<dyn Error>> {
    let mut rdr = csv::Reader::from_path(file_path)?;

    for result in rdr.deserialize() {
        let record: IbRecord = result?;

        portfolio.securities.entry(record.symbol.clone()).or_insert_with(|| Security {
            symbol: record.symbol.clone(),
            description: record.description,
        });

        let transaction = Transaction {
            symbol: record.symbol,
            kind: record.transaction_type,
            datetime: record.datetime,
            settle_date: record.settle_date,
            buy_sell: record.buy_sell,
            quantity: record.quantity,
            price: record.price,
            amount: record.amount,
            commission: record.commission
                + record.broker_execution_commission
                + record.broker_clearing_commission
                + record.other_commission
                + record.tax,
            currency: record.currency,
        };
        portfolio.transactions.push(transaction);
    }

    Ok(())
}
