//! How import stops are kept across runs of their importer. All values are
//! invented.

mod common;

use anyhow::Result;
use common::create_temp_db;
use db::stops::{self, Pick, Record, Stop};
use ledger_types::currency::Currency;
use rust_decimal_macros::dec;
use sqlx::SqlitePool;

/// A 天天記帳 spend an import could not place.
fn lunch(id: &str) -> Record {
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

fn unexplained(record: Record) -> Stop {
    Stop::RecordUnexplained {
        record,
        through: "2024-02-29".parse().unwrap(),
        lines: vec![],
        accounts: vec![],
    }
}

async fn refuse(pool: &SqlitePool, source: &str, found: &[Stop]) -> Result<()> {
    stops::record(&mut *pool.acquire().await?, source, found).await
}

/// Each open stop, and whether it carries a pick.
async fn open(pool: &SqlitePool) -> Result<Vec<(Stop, bool)>> {
    Ok(stops::open(pool).await?.into_iter().map(|o| (o.stop, o.pick.is_some())).collect())
}

/// A statement line a record can be picked for.
async fn line(pool: &SqlitePool) -> Result<i64> {
    Ok(sqlx::query("INSERT INTO transactions (date, source) VALUES ('2024-02-01', 'import')")
        .execute(pool)
        .await?
        .last_insert_rowid())
}

/// A refusal replaces what its importer left open, but a pick survives one
/// for another reason; met again, its stop drops the pick. Another importer's
/// stops are its own, and a run that succeeds clears them all.
#[tokio::test]
async fn each_run_replaces_what_its_importer_left_open() -> Result<()> {
    let (pool, _file) = create_temp_db().await?;
    let fee = line(&pool).await?;
    let picked = Stop::RecordAmbiguous { record: lunch("U1"), lines: vec![fee] };
    let other = unexplained(lunch("U2"));
    let bank =
        Stop::SeveralOpenings { account: "Assets:Bank:Savings".into(), currency: Currency::TWD };
    refuse(&pool, "tiantian", &[picked.clone(), other.clone()]).await?;
    refuse(&pool, "cathay-bank", &[bank]).await?;
    let id = stops::open(&pool).await?[0].id;
    stops::choose(&pool, id, &Pick::Line(fee)).await?;

    refuse(&pool, "tiantian", std::slice::from_ref(&other)).await?;
    assert_eq!(open(&pool).await?.len(), 3, "the pick and the bank's stop stay");
    refuse(&pool, "tiantian", std::slice::from_ref(&picked)).await?;
    let left = open(&pool).await?;
    assert!(left.iter().all(|(_, pick)| !pick), "{left:?}");
    assert!(!left.contains(&(other, false)), "{left:?}");

    stops::clear(&mut *pool.acquire().await?, "tiantian").await?;
    let left = open(&pool).await?;
    assert!(matches!(left.as_slice(), [(Stop::SeveralOpenings { .. }, false)]), "{left:?}");
    Ok(())
}

/// A record met again for another reason is one stop still: the picked row
/// for the old reason goes with its pick.
#[tokio::test]
async fn a_record_met_for_another_reason_replaces_its_stop() -> Result<()> {
    let (pool, _file) = create_temp_db().await?;
    let fee = line(&pool).await?;
    refuse(&pool, "tiantian", &[Stop::RecordAmbiguous { record: lunch("U1"), lines: vec![fee] }])
        .await?;
    let id = stops::open(&pool).await?[0].id;
    stops::choose(&pool, id, &Pick::Line(fee)).await?;

    refuse(&pool, "tiantian", &[unexplained(lunch("U1"))]).await?;
    assert_eq!(open(&pool).await?, [(unexplained(lunch("U1")), false)]);
    assert!(stops::picks(&mut *pool.acquire().await?, "tiantian").await?.is_empty());
    Ok(())
}
