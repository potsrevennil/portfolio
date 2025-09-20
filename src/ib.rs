use std::{collections::HashMap, error::Error};

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::{prelude::FromPrimitive, Decimal};
use serde::Deserialize;

use crate::stocks::{Broker, BuySell, Currency, Portfolio, Security, Transaction, TransactionKind};

mod de_utils {

    pub mod date_format {
        use chrono::NaiveDate;
        use serde::{self, Deserialize, Deserializer};

        const FORMAT: &'static str = "%Y%m%d";

        pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveDate, D::Error>
        where
            D: Deserializer<'de>,
        {
            let s = String::deserialize(deserializer)?;
            NaiveDate::parse_from_str(&s, FORMAT).map_err(serde::de::Error::custom)
        }
    }

    pub mod optional_date_format {
        use chrono::NaiveDate;
        use serde::{self, Deserialize, Deserializer};

        const FORMAT: &'static str = "%Y%m%d";

        pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<NaiveDate>, D::Error>
        where
            D: Deserializer<'de>,
        {
            let s = String::deserialize(deserializer)?;
            if s.is_empty() {
                Ok(None)
            } else {
                NaiveDate::parse_from_str(&s, FORMAT).map(Some).map_err(serde::de::Error::custom)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbRecord {
    #[serde(rename = "CurrencyPrimary")]
    currency: Currency,
    symbol: String,
    description: String,
    #[serde(with = "de_utils::date_format")]
    date: NaiveDate,
    #[serde(with = "de_utils::optional_date_format")]
    settle_date: Option<NaiveDate>,
    activity_description: String,
    #[serde(rename = "Buy/Sell")]
    buy_sell: Option<BuySell>,
    #[serde(rename = "TradeQuantity")]
    quantity: Decimal,
    #[serde(rename = "TradePrice")]
    price: Decimal,
    #[serde(rename = "TradeCommission")]
    commission: Decimal,
    #[serde(rename = "TradeTax")]
    tax: Decimal,
    amount: Decimal,
    balance: Decimal,
}

impl IbRecord {
    fn get_transaction_kind(&self) -> TransactionKind {
        let desc = &self.activity_description;
        if desc.contains("Dividend") {
            TransactionKind::Dividend
        } else if desc.contains("Tax") || desc.contains("Withholding") {
            TransactionKind::Tax
        } else if desc.contains("Interest") {
            TransactionKind::Interest
        } else if desc.contains("Fee") {
            TransactionKind::Fee
        } else if desc.starts_with("Buy") || desc.starts_with("Sell") {
            TransactionKind::Trade
        } else if desc == "Cash Transfer" || desc == "Electronic Fund Transfer" {
            if self.amount.is_sign_positive() {
                TransactionKind::Deposit
            } else {
                TransactionKind::Withdrawal
            }
        } else {
            TransactionKind::Other
        }
    }
}

pub fn load_from_ib_csv(portfolio: &mut Portfolio, file_path: &str) -> Result<(), Box<dyn Error>> {
    let mut rdr = csv::Reader::from_path(file_path)?;

    let mut running_cash_balance: HashMap<Currency, Decimal> = HashMap::new();

    let mut i = 0;
    for result in rdr.deserialize() {
        let record: IbRecord = result?;

        portfolio.securities.entry(record.symbol.clone()).or_insert_with(|| Security {
            symbol: record.symbol.clone(),
            description: record.description.clone(),
        });

        let kind = record.get_transaction_kind();
        let transaction = Transaction {
            source: Broker::InteractiveBrokers,
            symbol: record.symbol.clone(),
            kind,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: record.settle_date,
            buy_sell: record.buy_sell,
            quantity: record.quantity,
            price: if record.price.is_zero() { None } else { Some(record.price) },
            amount: record.amount,
            commission: record.commission + record.tax,
            currency: record.currency,
            balance: record.balance,
        };

        // Update running cash balance
        let current_currency_balance =
            running_cash_balance.entry(transaction.currency).or_insert(Decimal::ZERO);
        *current_currency_balance += transaction.amount;

        // Compare with record.balance
        // Use a small epsilon for floating point comparisons with Decimal
        let epsilon = Decimal::from_f64(0.00000001).unwrap(); // Define a small epsilon
        if (record.balance - *current_currency_balance).abs() > epsilon {
            println!("{i} {:?}", transaction);
            return Err(format!(
                "Balance mismatch for transaction on {}: Expected {}, got {}",
                transaction.datetime, record.balance, *current_currency_balance
            )
            .into());
        }

        portfolio.transactions.push(transaction);
        i += 1;
    }

    Ok(())
}
