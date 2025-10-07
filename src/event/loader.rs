use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::NaiveDate;

use crate::{
    event::{csv_loader, store},
    portfolio::portfolio::{Broker, Event, Security},
    split::{service::fixed_splits, store::SplitStore},
};

pub async fn load(
    input_file: Vec<String>,
    output_file: Option<String>,
    broker_type: Option<Broker>,
    split_store: &SplitStore,
) -> Result<(BTreeMap<NaiveDate, Event>, HashMap<String, Security>)> {
    // 1. Call csv_loader::load_from_csv to get initial transactions and securities.
    let (mut events, securities) = csv_loader::load_from_csv(input_file, broker_type).await?;

    // 2. Load hardcoded splits by calling fixed_splits() from split::service.
    let hardcoded_splits = fixed_splits();

    // 3. Merge the hardcoded splits with any splits obtained from the CSV files.
    for (date, splits_for_date) in hardcoded_splits {
        events.entry(date).or_default().splits.extend(splits_for_date);
    }

    // 4. Call store::store to save transactions to CSV and the *combined* splits to
    //    DB.
    store::store(&events, &securities, output_file, split_store).await?;

    // 5. Call store::load_splits to update the BTreeMap<NaiveDate, Event> with the
    //    latest split information from the database.
    let db_splits = store::load_splits(split_store).await?;
    for (date, splits_for_date) in db_splits {
        events.entry(date).or_default().splits.extend(splits_for_date);
    }

    Ok((events, securities))
}
