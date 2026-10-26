//! How import stops are kept across runs of their importer. All values are
//! invented.

mod common;

use anyhow::Result;
use common::{
    create_temp_db,
    stops::{refuse, unexplained},
};
use db::stops::{self, Stop};
use ledger_types::currency::Currency;

/// A refusal replaces what its importer left open, and leaves another
/// importer's alone; a run that succeeds clears its own.
#[tokio::test]
async fn each_run_replaces_what_its_importer_left_open() -> Result<()> {
    let (pool, _file) = create_temp_db().await?;
    let bank =
        Stop::SeveralOpenings { account: "Assets:Bank:Savings".into(), currency: Currency::TWD };
    refuse(&pool, "tiantian", &[unexplained("U1", vec![]), unexplained("U2", vec![])]).await;
    refuse(&pool, "cathay-bank", std::slice::from_ref(&bank)).await;

    refuse(&pool, "tiantian", &[unexplained("U2", vec![])]).await;
    assert_eq!(stops::open(&pool).await?, [unexplained("U2", vec![]), bank.clone()]);

    stops::clear(&mut *pool.acquire().await?, "tiantian").await?;
    assert_eq!(stops::open(&pool).await?, [bank]);
    Ok(())
}
