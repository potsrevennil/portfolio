use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::NaiveDate;
use csv;

use crate::{
    cathay, ib,
    portfolio::{
        portfolio::{Broker, Event, Security, Transaction},
        CsvTransactionRecord,
    },
};

pub fn load_from_generic_csv(
    file_path: &str,
) -> Result<HashMap<Broker, (BTreeMap<NaiveDate, Event>, HashMap<String, Security>)>> {
    let mut reader = csv::Reader::from_path(file_path)?;
    let mut broker_transactions: HashMap<Broker, Vec<CsvTransactionRecord>> = HashMap::new();

    for result in reader.deserialize() {
        let record: CsvTransactionRecord = result?;
        broker_transactions.entry(record.source).or_default().push(record);
    }

    let mut result_map: HashMap<Broker, (BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> =
        HashMap::new();

    for (broker, records) in broker_transactions {
        let mut events: BTreeMap<NaiveDate, Event> = BTreeMap::new();
        let mut securities: HashMap<String, Security> = HashMap::new();

        for record in records {
            let mut t = Transaction {
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

            if t.id.is_empty() {
                t.generate_id();
            }

            let date = t.datetime.date_naive();
            events.entry(date).or_default().transactions.push(t);

            if !record.symbol.is_empty() {
                securities.entry(record.symbol.clone()).or_insert(Security {
                    symbol: record.symbol.clone(),
                    description: record.description,
                });
            }
        }
        result_map.insert(broker, (events, securities));
    }

    Ok(result_map)
}

pub async fn load_from_csv(
    file_paths: Vec<String>,
    broker_type: Option<Broker>,
) -> Result<HashMap<Broker, (BTreeMap<NaiveDate, Event>, HashMap<String, Security>)>> {
    let mut result_map: HashMap<Broker, (BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> =
        HashMap::new();

    let mut m: HashMap<Broker, (BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> =
        HashMap::new();
    for file_path in file_paths {
        match broker_type {
            Some(Broker::InteractiveBrokers) => {
                let (events, securities) = ib::load_from_csv(&file_path)?;
                m.entry(Broker::InteractiveBrokers)
                    .and_modify(|(e, s)| {
                        e.extend(events.clone()); // merge BTreeMap
                        s.extend(securities.clone()); // merge HashMap
                    })
                    .or_insert((events, securities));
            }
            Some(Broker::Cathay) => {
                let (events, securities) = cathay::load_from_csv(&file_path)?;
                m.entry(Broker::Cathay)
                    .and_modify(|(e, s)| {
                        e.extend(events.clone()); // merge BTreeMap
                        s.extend(securities.clone()); // merge HashMap
                    })
                    .or_insert((events, securities));
            }
            _ => {
                // This case should ideally not be hit if we are loading from a generic CSV
                // that contains multiple brokers. The logic is now in load_from_generic_csv.
                // Let's assume a single file load here will be for a generic, single-broker
                // file if broker_type is None.
                let n = load_from_generic_csv(&file_path)?;
                for (broker, (events, securities)) in n {
                    m.entry(broker)
                        .and_modify(|(e, s)| {
                            e.extend(events.clone()); // merge BTreeMap
                            s.extend(securities.clone()); // merge HashMap
                        })
                        .or_insert((events, securities));
                }
            }
        };
    }

    for (broker, (events, securities)) in m {
        let (agg_events, agg_securities) = result_map.entry(broker).or_default();

        for (date, event) in events {
            agg_events.entry(date).or_default().transactions.extend(event.transactions);
            agg_events.entry(date).or_default().splits.extend(event.splits);
        }
        agg_securities.extend(securities);
    }

    Ok(result_map)
}

pub async fn store_transactions(
    events: &BTreeMap<NaiveDate, Event>,
    securities: &HashMap<String, Security>,
    output_file: Option<String>,
) -> Result<()> {
    // Write transactions to CSV if path is provided
    if let Some(file_path) = output_file {
        let mut writer = csv::Writer::from_path(file_path)?;
        for event in events.values() {
            let mut transactions = event.transactions.clone();
            transactions.sort_by_key(|t| t.datetime);

            for t in &transactions {
                let description =
                    securities.get(&t.symbol).map_or(String::new(), |s| s.description.clone());
                writer.serialize(CsvTransactionRecord {
                    id: t.id.clone(),
                    source: t.source,
                    asset_class: t.asset_class,
                    symbol: t.symbol.clone(),
                    description,
                    kind: t.kind,
                    datetime: t.datetime,
                    settle_date: t.settle_date,
                    quantity: t.quantity,
                    price: t.price,
                    amount: t.amount,
                    commission: t.commission,
                    currency: t.currency,
                    balance: t.balance,
                })?;
            }
        }
        writer.flush()?;
    }

    Ok(())
}
