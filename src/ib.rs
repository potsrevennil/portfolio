use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use regex;
use rust_decimal::Decimal;
use serde::Deserialize;

mod de_utils {
    use chrono::{DateTime, NaiveDate, Utc};
    use rust_decimal::Decimal;
    use serde::{self, de::Error, Deserialize, Deserializer};

    pub fn date_format<'de, D>(deserializer: D) -> Result<NaiveDate, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(D::Error::custom)
    }

    pub fn datetime_format<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let date_str = s.split(',').next().unwrap_or_default().trim();
        let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
            .map_err(|e| D::Error::custom(e.to_string()))?;

        date.and_hms_opt(
            s[12..14].parse().unwrap_or_default(), // Hour
            s[15..17].parse().unwrap_or_default(), // Minute
            s[18..20].parse().unwrap_or_default(), // Second
        )
        .ok_or_else(|| D::Error::custom("Invalid time format"))
        .map(|naive_datetime| DateTime::<Utc>::from_naive_utc_and_offset(naive_datetime, Utc))
        .map_err(|e| D::Error::custom(e.to_string()))
    }

    pub fn decimal_from_string<'de, D>(deserializer: D) -> Result<Decimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.trim().is_empty() || s.trim() == "--" {
            Ok(Decimal::ZERO)
        } else {
            s.replace(',', "").parse::<Decimal>().map_err(D::Error::custom)
        }
    }
}

use crate::stocks::{
    AssetClass, Broker, Currency, Portfolio, Security, Transaction, TransactionKind,
};

// --- Trades ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbTradeRecord {
    #[serde(rename = "DataDiscriminator")]
    _data_discriminator: String, // e.g., "Order"
    #[serde(rename = "Asset Category")]
    asset_category: AssetClass, // e.g., "Stocks"
    currency: Currency, // e.g., "USD"
    _account: String,   // e.g., "U1234567"
    symbol: String,
    #[serde(rename = "Date/Time", deserialize_with = "de_utils::datetime_format")]
    date_time: DateTime<Utc>,
    #[serde(rename = "Quantity")]
    quantity: Decimal,
    #[serde(rename = "T. Price")]
    t_price: Decimal, // Trade Price
    #[serde(rename = "C. Price")]
    _c_price: Decimal, // Close Price
    proceeds: Decimal,
    #[serde(rename = "Comm/Fee")]
    comm_fee: Decimal,
    _basis: Decimal,
    #[serde(rename = "Realized P/L")]
    _realized_pnl: Decimal,
    #[serde(rename = "MTM P/L")]
    _mtm_pnl: Decimal,
    _code: String, // e.g., "IA;O", "C"
}

// --- Grant Activity ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbGrantActivityRecord {
    _account: String,
    symbol: String,
    #[serde(rename = "Report Date", deserialize_with = "de_utils::date_format")]
    _report_date: NaiveDate,
    _description: String,
    #[serde(rename = "Award Date", deserialize_with = "de_utils::date_format")]
    award_date: NaiveDate,
    #[serde(rename = "Vesting Date", deserialize_with = "de_utils::date_format")]
    vesting_date: NaiveDate,
    quantity: Decimal,
    _price: Decimal,
    _value: Decimal,
}

// --- Transfers ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbTransferRecord {
    #[serde(rename = "Asset Category")]
    asset_category: AssetClass,
    currency: Currency,
    _account: String,
    symbol: String,
    #[serde(deserialize_with = "de_utils::date_format")]
    date: NaiveDate,
    #[serde(rename = "Type")]
    _transfer_type: String, // e.g., "ACATS"
    direction: String, // e.g., "In", "Out"
    #[serde(rename = "Xfer Company")]
    _xfer_company: String,
    #[serde(rename = "Xfer Account")]
    _xfer_account: String,
    #[serde(rename = "Qty")]
    quantity: Decimal,
    #[serde(rename = "Xfer Price", deserialize_with = "de_utils::decimal_from_string")]
    _xfer_price: Decimal, // Can be "--"
    #[serde(rename = "Market Value", deserialize_with = "de_utils::decimal_from_string")]
    market_value: Decimal, // Can contain commas
    #[serde(rename = "Realized P/L")]
    _realized_pnl: Decimal,
    #[serde(rename = "Cash Amount")]
    _cash_amount: Decimal,
    _code: String,
}

// --- Deposits & Withdrawals ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbDepositWithdrawalRecord {
    currency: Currency,
    _account: String,
    #[serde(rename = "Settle Date", deserialize_with = "de_utils::date_format")]
    settle_date: NaiveDate,
    _description: String,
    amount: Decimal,
}

// --- Fees ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbFeeRecord {
    _subtitle: String, // e.g., "Other Fees"
    currency: Currency,
    _account: String,
    #[serde(deserialize_with = "de_utils::date_format")]
    date: NaiveDate,
    description: String,
    amount: Decimal,
}

// --- Dividends ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbDividendRecord {
    currency: Currency,
    _account: String,
    #[serde(deserialize_with = "de_utils::date_format")]
    date: NaiveDate,
    description: String, // Contains symbol and amount details
    amount: Decimal,
}

// --- Withholding Tax ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbWithholdingTaxRecord {
    currency: Currency,
    _account: String,
    #[serde(deserialize_with = "de_utils::date_format")]
    date: NaiveDate,
    description: String, // Contains symbol details
    amount: Decimal,
    _code: String,
}

// --- Interest ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbInterestRecord {
    currency: Currency,
    _account: String,
    #[serde(deserialize_with = "de_utils::date_format")]
    date: NaiveDate,
    _description: String,
    amount: Decimal,
}

// --- Corporate Actions ---
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct IbCorporateActionRecord {
    #[serde(rename = "Asset Category")]
    asset_category: AssetClass,
    currency: Currency,
    _account: String,
    #[serde(rename = "Report Date", deserialize_with = "de_utils::date_format")]
    _report_date: NaiveDate,
    #[serde(rename = "Date/Time", deserialize_with = "de_utils::datetime_format")]
    date_time: DateTime<Utc>,
    description: String, // e.g., "IBKR(US45841N1072) Split 4 for 1"
    quantity: Decimal,
    _proceeds: Decimal,
    _value: Decimal,
    #[serde(rename = "Realized P/L")]
    _realized_pnl: Decimal,
    _code: String,
}

pub fn load_from_ib_csv(portfolio: &mut Portfolio, file_path: &str) -> Result<()> {
    let mut reader =
        csv::ReaderBuilder::new().has_headers(false).flexible(true).from_path(file_path)?;

    let mut headers: HashMap<String, csv::StringRecord> = HashMap::new();
    let mut current_section: Option<String> = None;
    let mut transactions: Vec<Transaction> = Vec::new();
    let mut withholding_tax_records: Vec<IbWithholdingTaxRecord> = Vec::new();

    for result in reader.records() {
        let record = result.context("Failed to read record from CSV")?;

        let section_type = record.get(0).unwrap_or_default();
        let row_type = record.get(1).unwrap_or_default();
        let sub_row_type = record.get(2).unwrap_or_default();

        match (row_type, sub_row_type) {
            ("SubTotal", _) | ("Total", _) | (_, "SubTotal") | (_, "Total") => {
                // Ignore summary rows
            }
            ("Header", _) => {
                current_section = Some(section_type.to_string());
                headers.insert(section_type.to_string(), record.clone());
            }
            ("Data", _) => {
                if let Some(section) = &current_section {
                    let header = headers
                        .get(section)
                        .context(format!("Header not found for section: {}", section))?;

                    match section.as_str() {
                        "Trades" => {
                            let r = record.deserialize::<IbTradeRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        "Grant Activity" => {
                            let r = record.deserialize::<IbGrantActivityRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        "Transfers" => {
                            let r = record.deserialize::<IbTransferRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        "Deposits & Withdrawals" => {
                            let r =
                                record.deserialize::<IbDepositWithdrawalRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        "Fees" => {
                            let r = record.deserialize::<IbFeeRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        "Dividends" => {
                            let r = record.deserialize::<IbDividendRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        "Withholding Tax" => {
                            let r = record.deserialize::<IbWithholdingTaxRecord>(Some(header))?;
                            withholding_tax_records.push(r);
                        }
                        "Interest" => {
                            let r = record.deserialize::<IbInterestRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        "Corporate Actions" => {
                            let r = record.deserialize::<IbCorporateActionRecord>(Some(header))?;
                            transactions.push(r.into());
                        }
                        _ => {
                            // Ignore other sections
                        }
                    }
                }
            }
            _ => {
                // Potentially a new section without a header, or an unexpected
                // row type For now, we'll just continue, but
                // could add more robust error handling
                // or logging here if needed.
            }
        }
    }

    // Process grouped withholding tax records
    let grouped_withholding_tax = group_withholding_tax_records(withholding_tax_records);
    for tax_record in grouped_withholding_tax {
        transactions.push(tax_record.into());
    }

    // Sort all transactions by datetime
    transactions.sort_by_key(|t| t.datetime);

    // Populate portfolio and generate unique IDs
    let mut transaction_ids = std::collections::HashSet::new();
    for mut tx in transactions {
        // Generate unique ID using gxhash
        let id_string = format!(
            "{}-{:?}-{:?}-{}-{:?}-{:?}",
            tx.datetime.to_rfc3339(),
            tx.source,
            tx.kind,
            tx.symbol,
            tx.quantity,
            tx.amount
        );
        use gxhash::gxhash64;

        // ... (rest of the file)

        tx.id = format!("{:x}", gxhash64(id_string.as_bytes(), 0));

        if !transaction_ids.insert(tx.id.clone()) {
            eprintln!("Warning: Duplicate transaction ID generated, skipping record: {}", tx.id);
            continue;
        }

        if !tx.symbol.is_empty() {
            portfolio.securities.entry(tx.symbol.clone()).or_insert_with(|| Security {
                symbol: tx.symbol.clone(),
                description: "".to_string(), /* Description will be updated from Financial
                                              * Instrument
                                              * Information later */
            });
        }
        portfolio.transactions.push(tx);
    }

    Ok(())
}

// Helper function to group withholding tax records
fn group_withholding_tax_records(
    records: Vec<IbWithholdingTaxRecord>,
) -> Vec<IbWithholdingTaxRecord> {
    let mut grouped: HashMap<(NaiveDate, String, Currency), IbWithholdingTaxRecord> =
        HashMap::new();

    for record in records {
        let key = (record.date, record.description.clone(), record.currency);
        grouped
            .entry(key)
            .and_modify(|r| {
                r.amount += record.amount;
            })
            .or_insert(record);
    }

    grouped.into_values().collect()
}

// --- Conversion Implementations (TryFrom for better error handling) ---

impl From<IbTradeRecord> for Transaction {
    fn from(record: IbTradeRecord) -> Self {
        let kind = if record.quantity.is_sign_positive() {
            TransactionKind::Buy
        } else {
            TransactionKind::Sell
        };
        let asset_class = record.asset_category;
        let currency = record.currency;

        Transaction {
            id: String::new(), // ID will be generated later
            source: Broker::InteractiveBrokers,
            asset_class,
            symbol: record.symbol,
            kind,
            datetime: record.date_time,
            settle_date: Some(record.date_time.date_naive()), /* Assuming settle_date is trade
                                                               * date for now */
            quantity: record.quantity.abs(),
            price: record.t_price,
            amount: record.proceeds,
            commission: record.comm_fee,
            currency,
            balance: Decimal::ZERO, // Balance will be calculated later
        }
    }
}

impl From<IbGrantActivityRecord> for Transaction {
    fn from(record: IbGrantActivityRecord) -> Self {
        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class: AssetClass::Stocks,
            symbol: record.symbol,
            kind: TransactionKind::Deposit,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.award_date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.vesting_date),
            quantity: record.quantity,
            price: Decimal::ZERO,
            amount: Decimal::ZERO,
            commission: Decimal::ZERO,
            currency: Currency::USD, // Assuming USD for grant activity
            balance: Decimal::ZERO,
        }
    }
}

impl From<IbTransferRecord> for Transaction {
    fn from(record: IbTransferRecord) -> Self {
        let kind = if record.direction == "In" {
            TransactionKind::Deposit
        } else {
            TransactionKind::Withdrawal
        };
        let asset_class = record.asset_category;
        let currency = record.currency;

        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class,
            symbol: record.symbol,
            kind,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.date),
            quantity: record.quantity,
            price: record.market_value.checked_div(record.quantity).unwrap_or_default(),
            amount: record.market_value, // Use market_value for stock transfers
            commission: Decimal::ZERO,
            currency,
            balance: Decimal::ZERO,
        }
    }
}

impl From<IbDepositWithdrawalRecord> for Transaction {
    fn from(record: IbDepositWithdrawalRecord) -> Self {
        let kind = if record.amount.is_sign_positive() {
            TransactionKind::Deposit
        } else {
            TransactionKind::Withdrawal
        };
        let currency = record.currency;

        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class: AssetClass::Cash,
            symbol: String::new(), // No symbol for cash transactions
            kind,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.settle_date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.settle_date),
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount: record.amount,
            commission: Decimal::ZERO,
            currency,
            balance: Decimal::ZERO,
        }
    }
}

impl From<IbFeeRecord> for Transaction {
    fn from(record: IbFeeRecord) -> Self {
        let currency = record.currency;
        let symbol = extract_symbol_from_description(&record.description).unwrap_or_default();

        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class: AssetClass::Cash,
            symbol,
            kind: TransactionKind::Fee,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.date),
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount: record.amount,
            commission: Decimal::ZERO,
            currency,
            balance: Decimal::ZERO,
        }
    }
}

impl From<IbDividendRecord> for Transaction {
    fn from(record: IbDividendRecord) -> Self {
        let currency = record.currency;
        let symbol = extract_symbol_from_description(&record.description).unwrap_or_default();

        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class: AssetClass::Cash,
            symbol,
            kind: TransactionKind::Dividend,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.date),
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount: record.amount,
            commission: Decimal::ZERO,
            currency,
            balance: Decimal::ZERO,
        }
    }
}

impl From<IbWithholdingTaxRecord> for Transaction {
    fn from(record: IbWithholdingTaxRecord) -> Self {
        let currency = record.currency;
        let symbol = extract_symbol_from_description(&record.description).unwrap_or_default();

        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class: AssetClass::Cash,
            symbol,
            kind: TransactionKind::Tax,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.date),
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount: record.amount,
            commission: Decimal::ZERO,
            currency,
            balance: Decimal::ZERO,
        }
    }
}

impl From<IbInterestRecord> for Transaction {
    fn from(record: IbInterestRecord) -> Self {
        let currency = record.currency;

        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class: AssetClass::Cash,
            symbol: String::new(), // No symbol for general interest
            kind: TransactionKind::Interest,
            datetime: DateTime::<Utc>::from_naive_utc_and_offset(
                record.date.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            ),
            settle_date: Some(record.date),
            quantity: Decimal::ZERO,
            price: Decimal::ZERO,
            amount: record.amount,
            commission: Decimal::ZERO,
            currency,
            balance: Decimal::ZERO,
        }
    }
}

impl From<IbCorporateActionRecord> for Transaction {
    fn from(record: IbCorporateActionRecord) -> Self {
        let _asset_class = record.asset_category;
        let currency = record.currency;
        let symbol = extract_symbol_from_description(&record.description).unwrap_or_default();

        Transaction {
            id: String::new(),
            source: Broker::InteractiveBrokers,
            asset_class: AssetClass::Stocks,
            symbol,
            kind: TransactionKind::CorporateAction,
            datetime: record.date_time,
            settle_date: Some(record.date_time.date_naive()),
            quantity: record.quantity,
            price: Decimal::ZERO,
            amount: Decimal::ZERO,
            commission: Decimal::ZERO,
            currency,
            balance: Decimal::ZERO,
        }
    }
}

// Helper to extract symbol from descriptions like "AAPL(US0378331005) Cash
// Dividend"
fn extract_symbol_from_description(description: &str) -> Option<String> {
    let re = regex::Regex::new(r"^([A-Z]+)\(.+\)").ok()?;
    re.captures(description).and_then(|caps| caps.get(1).map(|m| m.as_str().to_string()))
}
