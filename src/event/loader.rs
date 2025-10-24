use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::NaiveDate;

use crate::{
    event::{csv_loader, store},
    portfolio::portfolio::{Broker, Event, Security},
    split::{service::fixed_splits, store::SplitStore},
};

pub enum DataSource {
    Ib(Vec<String>),
    Cathay(Vec<String>),
    Generic(Vec<String>),
}

pub async fn load(
    sources: Vec<DataSource>,
    output_file: Option<String>,
    split_store: &SplitStore,
) -> Result<HashMap<Broker, (BTreeMap<NaiveDate, Event>, HashMap<String, Security>)>> {
    let mut broker_data: HashMap<Broker, (BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> =
        HashMap::new();

    // --- Loading Phase ---
    for source in sources {
        let source_broker_data = match source {
            DataSource::Ib(files) => {
                csv_loader::load_from_csv(files, Some(Broker::InteractiveBrokers)).await?
            }
            DataSource::Cathay(files) => {
                csv_loader::load_from_csv(files, Some(Broker::Cathay)).await?
            }
            DataSource::Generic(files) => csv_loader::load_from_csv(files, None).await?,
        };

        for (broker, (events, securities)) in source_broker_data {
            let (agg_events, agg_securities) = broker_data.entry(broker).or_default();
            for (date, event) in events {
                agg_events.entry(date).or_default().transactions.extend(event.transactions);
                agg_events.entry(date).or_default().splits.extend(event.splits);
            }
            agg_securities.extend(securities);
        }
    }

    // --- Prepare for Storing (Transactions and Splits) ---
    let hardcoded_splits = fixed_splits();
    let mut all_events_for_storing: BTreeMap<NaiveDate, Event> = BTreeMap::new();
    let mut all_securities_for_storing: HashMap<String, Security> = HashMap::new();

    for (_broker, (events, securities)) in &broker_data {
        for (date, event) in events {
            all_events_for_storing
                .entry(*date)
                .or_default()
                .transactions
                .extend(event.transactions.clone());
            all_events_for_storing.entry(*date).or_default().splits.extend(event.splits.clone());
        }
        all_securities_for_storing.extend(securities.clone());
    }
    // Merge hardcoded splits into the collection for storing
    for (date, splits) in hardcoded_splits {
        all_events_for_storing.entry(date).or_default().splits.extend(splits);
    }

    // --- Storing Phase ---
    store::store_splits(&all_events_for_storing, split_store).await?;
    if let Some(output_path) = output_file {
        csv_loader::store_transactions(
            &all_events_for_storing,
            &all_securities_for_storing,
            Some(output_path),
        )
        .await?;
    }

    // --- Inject Definitive Splits into broker_data for return ---
    let all_db_splits_final = store::load_splits(split_store).await?;
    for (_broker, (events, _securities)) in broker_data.iter_mut() {
        for event in events.values_mut() {
            event.splits.clear(); // Clear any old split info
        }
        for (date, splits) in &all_db_splits_final {
            events.entry(*date).or_default().splits.extend(splits.clone());
        }
    }

    Ok(broker_data)
}
