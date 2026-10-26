//! Import stops over invented records, as a refused import keeps them. Not
//! every test binary uses every item.
#![allow(dead_code)]

use db::stops::{self, Record, Stop};
use ledger_types::currency::Currency;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

/// A 天天記帳 lunch paid from 活存 that an import could not place.
pub fn lunch(id: &str) -> Record {
    Record {
        external_ref: format!("tiantian:{id}"),
        date: "2024-02-01".parse().unwrap(),
        account: "Assets:Bank:Savings".into(),
        amount: dec!(-500),
        currency: Currency::TWD,
        counter: vec!["Expenses:Food".into()],
        narration: "午餐".into(),
    }
}

/// The statements reach the lunch's day, and no line of `lines` is it.
pub fn unexplained(id: &str, lines: Vec<i64>) -> Stop {
    Stop::RecordUnexplained { record: lunch(id), through: "2024-02-29".parse().unwrap(), lines }
}

/// What a refused run of `source` keeps.
pub async fn refuse(pool: &SqlitePool, source: &str, found: &[Stop]) {
    stops::record(&mut *pool.acquire().await.unwrap(), source, found).await.unwrap();
}
