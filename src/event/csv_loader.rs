use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::NaiveDate;

use crate::{
    cathay, ib,
    portfolio::{
        portfolio::{Broker, Event, Security, Transaction},
        CsvTransactionRecord,
    },
};

pub fn load_from_generic_csv(
    file_path: &str,
) -> Result<(BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> {
    let mut reader = csv::Reader::from_path(file_path)?;
    let mut transactions: BTreeMap<NaiveDate, Vec<Transaction>> = BTreeMap::new();
    let mut securities: HashMap<String, Security> = HashMap::new();
    let mut events: BTreeMap<NaiveDate, Event> = BTreeMap::new();

    for result in reader.deserialize() {
        let record: CsvTransactionRecord = result?;
        let t = Transaction {
            id: record.id,
            source: record.source,
            asset_class: record.asset_class,
            symbol: record.symbol.clone(),
            kind: record.kind,
            datetime: record.datetime,
            settle_date: record.settle_date,
            quantity: record.quantity,
            price: record.price,
            amount: record.amount,
            commission: record.commission,
            currency: record.currency,
            balance: record.balance,
        };

        let date = t.datetime.date_naive();
        transactions.entry(date).or_default().push(t);

        // Only insert into securities map if the symbol is not empty
        if !record.symbol.is_empty() {
            securities.entry(record.symbol.clone()).or_insert(Security {
                symbol: record.symbol.clone(),
                description: record.description,
            });
        }
    }

    for (d, ts) in transactions {
        let es = events.entry(d).or_insert_with(|| Event::default());
        es.transactions.extend(ts);
    }

    Ok((events, securities))
}

pub async fn load_from_csv(
    file_paths: Vec<String>,
    broker_type: Option<Broker>,
) -> Result<(BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> {
    let mut aggregated_events: BTreeMap<NaiveDate, Event> = BTreeMap::new();
    let mut aggregated_securities: HashMap<String, Security> = HashMap::new();

    for file_path in file_paths {
        let (events, securities) = match broker_type {
            Some(Broker::InteractiveBrokers) => ib::load_from_csv(&file_path)?,
            Some(Broker::Cathay) => cathay::load_from_csv(&file_path)?,
            // If broker_type is None, or an unsupported broker, default to generic CSV
            _ => load_from_generic_csv(&file_path)?,
        };

        // Aggregate events
        for (date, event) in events {
            aggregated_events.entry(date).or_default().transactions.extend(event.transactions);
            aggregated_events.entry(date).or_default().splits.extend(event.splits);
        }

        // Aggregate securities
        aggregated_securities.extend(securities);
    }

    Ok((aggregated_events, aggregated_securities))
}
