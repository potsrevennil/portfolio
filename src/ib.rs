use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::{prelude::FromPrimitive, Decimal};
use serde::Deserialize;

use crate::stocks::{Broker, Currency, Portfolio, Security, Transaction, TransactionKind};

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "UPPERCASE")]
enum BuySell {
    Buy,
    Sell,
}

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbTransferRecord {
    #[serde(rename = "CurrencyPrimary")]
    currency: Currency,
    symbol: String,
    description: String,
    #[serde(with = "de_utils::date_format")]
    date: NaiveDate,
    #[serde(with = "de_utils::optional_date_format")]
    settle_date: Option<NaiveDate>,
    quantity: Decimal,
    #[serde(rename = "PositionAmount")]
    position_amount: Decimal,
}

enum ParserType {
    IbRecord,
    IbTransferRecord,
    Unknown,
}

impl IbRecord {
    fn get_transaction_kind(&self) -> TransactionKind {
        let desc = &self.activity_description;
        match self.buy_sell {
            Some(BuySell::Buy) => TransactionKind::Buy,
            Some(BuySell::Sell) => TransactionKind::Sell,
            None => {
                if desc.contains("Dividend") {
                    TransactionKind::Dividend
                } else if desc.contains("Tax") || desc.contains("Withholding") {
                    TransactionKind::Tax
                } else if desc.contains("Interest") {
                    TransactionKind::Interest
                } else if desc.contains("Fee") {
                    TransactionKind::Fee
                } else if desc.starts_with("Buy") {
                    TransactionKind::Buy
                } else if desc.starts_with("Sell") {
                    TransactionKind::Sell
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
    }
}

pub fn load_from_ib_csv(portfolio: &mut Portfolio, file_path: &str) -> Result<()> {
    let records = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_path(file_path)?
        .into_records();

    let mut current_parser_type = ParserType::Unknown;
    let mut ib_record_header: Option<csv::StringRecord> = None;
    let mut ib_transfer_record_header: Option<csv::StringRecord> = None;

    for (i, record_result) in records.into_iter().enumerate() {
        let record_string = record_result.context(format!("Failed to read record from CSV {i}"))?;

        // Attempt to detect header
        if record_string.iter().any(|field| field == "Buy/Sell") {
            current_parser_type = ParserType::IbRecord;
            ib_record_header = Some(record_string);
            continue; // Skip header row
        } else if record_string.iter().any(|field| field == "TransferPrice") {
            current_parser_type = ParserType::IbTransferRecord;
            ib_transfer_record_header = Some(record_string);
            continue; // Skip header row
        }

        // Process data row based on current_parser_type
        match current_parser_type {
            ParserType::IbRecord => {
                let header = ib_record_header.as_ref().context("IbRecord header not found")?;
                let ib_record: IbRecord = record_string.deserialize(Some(header)).context(
                    format!("Failed to deserialize row as IbRecord: {:?}", record_string),
                )?;

                portfolio.securities.entry(ib_record.symbol.clone()).or_insert_with(|| Security {
                    symbol: ib_record.symbol.clone(),
                    description: ib_record.description.clone(),
                });

                let kind = ib_record.get_transaction_kind();
                let transaction = Transaction {
                    source: Broker::InteractiveBrokers,
                    symbol: ib_record.symbol.clone(),
                    kind,
                    datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                        ib_record.date.and_hms_opt(0, 0, 0).unwrap(),
                        Utc,
                    ),
                    settle_date: ib_record.settle_date,
                    quantity: ib_record.quantity,
                    price: ib_record.price,
                    amount: ib_record.amount,
                    commission: ib_record.commission + ib_record.tax,
                    currency: ib_record.currency,
                    balance: ib_record.balance,
                };

                // Update running cash balance
                let balance =
                    portfolio.cash_balances.entry(transaction.currency).or_insert(Decimal::ZERO);
                *balance += transaction.amount;

                // Compare with record.balance
                let epsilon = Decimal::from_f64(0.00000001).unwrap();
                ensure!(
                    (ib_record.balance - *balance).abs() <= epsilon,
                    "Balance mismatch for transaction on {}: Expected {}, got {}",
                    transaction.datetime,
                    ib_record.balance,
                    *balance
                );

                portfolio.transactions.push(transaction);
            }
            ParserType::IbTransferRecord => {
                let header = ib_transfer_record_header
                    .as_ref()
                    .context("IbTransferRecord header not found")?;
                let ib_transfer_record: IbTransferRecord =
                    record_string.deserialize(Some(header)).context(format!(
                        "Failed to deserialize row as IbTransferRecord: {:?}",
                        record_string
                    ))?;

                portfolio.securities.entry(ib_transfer_record.symbol.clone()).or_insert_with(
                    || Security {
                        symbol: ib_transfer_record.symbol.clone(),
                        description: ib_transfer_record.description.clone(),
                    },
                );

                let balance = portfolio
                    .cash_balances
                    .entry(ib_transfer_record.currency)
                    .or_insert(Decimal::ZERO);
                let transaction = Transaction {
                    source: Broker::InteractiveBrokers,
                    symbol: ib_transfer_record.symbol.clone(),
                    kind: TransactionKind::Deposit,
                    datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                        ib_transfer_record.date.and_hms_opt(0, 0, 0).unwrap(),
                        Utc,
                    ),
                    settle_date: ib_transfer_record.settle_date,
                    quantity: ib_transfer_record.quantity,
                    price: ib_transfer_record
                        .position_amount
                        .checked_div(ib_transfer_record.quantity)
                        .unwrap_or_default(),
                    amount: ib_transfer_record.position_amount,
                    commission: Decimal::ZERO,
                    currency: ib_transfer_record.currency,
                    balance: *balance,
                };
                portfolio.transactions.push(transaction);
            }
            ParserType::Unknown => {
                bail!("Encountered data row before a recognized header: {:?}", record_string);
            }
        }
    }

    Ok(())
}
