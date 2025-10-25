use std::{
    fmt::{Debug, Display},
    fs::OpenOptions,
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, NaiveDateTime};
use console::style;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use rust_decimal::Decimal;
use strum::IntoEnumIterator;

use crate::portfolio::portfolio::{
    AssetClass, Broker, CsvTransactionRecord, Transaction, TransactionKind,
};

pub fn ask_with_parser<T, F, E>(
    theme: &ColorfulTheme,
    prompt: &str,
    default_str: String,
    parser: F,
) -> Result<T>
where
    F: Fn(&str) -> Result<T, E>,
    E: Debug,
{
    loop {
        let value = Input::<String>::with_theme(theme)
            .with_prompt(prompt)
            .default(default_str.clone())
            .interact_text()?;

        match parser(&value) {
            Ok(val) => return Ok(val),
            Err(e) => {
                println!("{}: {:?}", style("Invalid input").red(), e);
                continue;
            }
        }
    }
}

fn ask<T>(theme: &ColorfulTheme, prompt: &str) -> Result<T>
where
    T: FromStr + ToString + Default,
    <T as FromStr>::Err: Debug,
{
    ask_with_parser(theme, prompt, T::default().to_string(), |s| s.parse::<T>())
}

fn ask_text(theme: &ColorfulTheme, prompt: &str) -> Result<String> {
    Input::<String>::with_theme(theme)
        .with_prompt(prompt)
        .default("".to_string())
        .interact_text()
        .map_err(anyhow::Error::from)
}

pub fn ask_select<T: Display>(theme: &ColorfulTheme, prompt: &str, items: &[T]) -> Result<usize> {
    Select::with_theme(theme)
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact()
        .map_err(anyhow::Error::from)
}

pub async fn run_interactive_record_session(file_path: &str) -> Result<()> {
    let theme = ColorfulTheme::default();
    let brokers: Vec<Broker> = Broker::iter().collect();
    let transaction_kinds: Vec<TransactionKind> = TransactionKind::iter().collect();
    let assets: Vec<AssetClass> = AssetClass::iter().collect();

    let broker_idx = ask_select(&theme, "Broker", &brokers)?;
    let broker = brokers[broker_idx];

    let kind_idx = ask_select(&theme, "Transaction type", &transaction_kinds)?;
    let kind = transaction_kinds[kind_idx];

    let (asset_class, symbol, quantity, price, amount, commission) = match kind {
        TransactionKind::Buy | TransactionKind::Sell | TransactionKind::CorporateAction => {
            let symbol = ask_text(&theme, "Symbol")?;
            let quantity = ask(&theme, "Quantity")?;
            let price = ask(&theme, "Price")?;
            let commission = ask(&theme, "Commission")?;
            let amount = quantity * price;
            (AssetClass::Stocks, symbol, quantity, price, amount, commission)
        }
        TransactionKind::Deposit | TransactionKind::Withdrawal => {
            let asset_idx = ask_select(&theme, "Asset type", &assets)?;
            let asset = assets[asset_idx];
            match asset {
                AssetClass::Cash => {
                    let amount = ask(&theme, "Amount")?;
                    (asset, String::new(), Decimal::ZERO, Decimal::ZERO, amount, Decimal::ZERO)
                }
                AssetClass::Stocks => {
                    let symbol = ask_text(&theme, "Symbol")?;
                    let quantity = ask(&theme, "Quantity")?;
                    (asset, symbol, quantity, Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
                }
            }
        }
        TransactionKind::Dividend => {
            let symbol = ask_text(&theme, "Symbol")?;
            let amount = ask(&theme, "Amount")?;
            (AssetClass::Cash, symbol, Decimal::ZERO, Decimal::ZERO, amount, Decimal::ZERO)
        }
        _ => {
            let amount = ask(&theme, "Amount")?;
            (AssetClass::Cash, String::new(), Decimal::ZERO, Decimal::ZERO, amount, Decimal::ZERO)
        }
    };

    let datetime = ask_with_parser(
        &theme,
        "Date and time",
        Local::now().naive_local().format("%Y-%m-%d %H:%M:%S").to_string(),
        |s| {
            if s.contains(' ') {
                NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                    .with_context(|| "Expected format: YYYY-MM-DD or YYYY-MM-DD HH:MM:SS")
            } else {
                NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map(|d| d.and_hms_opt(0, 0, 0).unwrap())
                    .with_context(|| "Expected format: YYYY-MM-DD or YYYY-MM-DD HH:MM:SS")
            }
        },
    )?;

    let description = ask_text(&theme, "Enter description (optional)")?;

    // TODO: also ask for this
    let currency = broker.reporting_currency();

    let mut transaction = Transaction {
        id: String::new(),
        source: broker,
        asset_class,
        symbol: symbol.clone(),
        kind,
        datetime: datetime.and_utc(),
        settle_date: Some(datetime.date()),
        quantity,
        price,
        amount: if kind == TransactionKind::Buy { -amount.abs() } else { amount.abs() },
        commission,
        currency,
        balance: Decimal::ZERO, // Not used when appending
    };

    transaction.generate_id();

    println!("\n{}", transaction);
    if !description.is_empty() {
        println!("{:<15}: {}", style("Description").bold(), description);
    }

    println!("\n");

    if Confirm::with_theme(&theme).with_prompt("Save this transaction?").default(true).interact()? {
        let csv_record = CsvTransactionRecord {
            id: transaction.id,
            source: transaction.source,
            asset_class: transaction.asset_class,
            symbol: transaction.symbol,
            description, // Use transaction's description
            kind: transaction.kind,
            datetime: transaction.datetime,
            settle_date: transaction.settle_date,
            quantity: transaction.quantity,
            price: transaction.price,
            amount: transaction.amount,
            commission: transaction.commission,
            currency: transaction.currency,
            balance: transaction.balance,
        };

        append_transaction_to_csv(file_path, csv_record)?;

        println!("✅ Transaction added to {}.\n", file_path);
    } else {
        println!("Transaction not saved.");
    }

    Ok(())
}

fn append_transaction_to_csv(file_path: &str, record: CsvTransactionRecord) -> Result<()> {
    let file_exists = Path::new(file_path).exists();

    let file = OpenOptions::new().write(true).create(true).append(true).open(file_path)?;

    let mut wtr = csv::WriterBuilder::new().has_headers(!file_exists).from_writer(file);

    wtr.serialize(record)?;
    wtr.flush()?;

    Ok(())
}
