use db::bill_split::{self, Attach, ClosedFilter, Closing, WriteOff};

use crate::fixture::*;

async fn closed_ids(l: &Ledger, filter: &ClosedFilter) -> Vec<i64> {
    bill_split::closed(&l.pool, filter).await.unwrap().iter().map(|c| c.split.id).collect()
}

/// Three closed splits and one open: repaid by Alice in 2025, partly written
/// off for Bob in 2026, written off on Alice's gear account in 2026.
async fn history(l: &Ledger) -> (i64, i64, i64, i64) {
    let (repaid, partly, gear, open) = (
        l.per_person("還清").await,
        l.per_person("部分").await,
        l.per_person("裝備").await,
        l.per_person("未清").await,
    );
    let share = Attach::Share(None);
    let line = l.line("2025-01-01", &[(BANK, "-100"), (ALICE, "100")]).await;
    bill_split::attach(&l.pool, &l.books, repaid, line, &share).await.unwrap();
    let back = l.line("2025-03-01", &[(BANK, "100"), (ALICE, "-100")]).await;
    bill_split::gather(&l.pool, &l.books, repaid, &[back]).await.unwrap();

    let line = l.line("2026-01-01", &[(BANK, "-200"), (BOB, "200")]).await;
    bill_split::attach(&l.pool, &l.books, partly, line, &share).await.unwrap();
    let back = l.line("2026-02-01", &[(BANK, "150"), (BOB, "-150")]).await;
    bill_split::gather(&l.pool, &l.books, partly, &[back]).await.unwrap();
    let off = |account| WriteOff { date: d(2026, 5, 1), ..off(account, None) };
    bill_split::write_off(&l.pool, &l.books, partly, &off(BOB)).await.unwrap();

    let line = l.line("2026-01-05", &[(BANK, "-70"), (ALICE_GEAR, "70")]).await;
    bill_split::attach(&l.pool, &l.books, gear, line, &share).await.unwrap();
    bill_split::write_off(&l.pool, &l.books, gear, &off(ALICE_GEAR)).await.unwrap();

    let line = l.line("2026-07-01", &[(BANK, "-90"), (ALICE, "90")]).await;
    bill_split::attach(&l.pool, &l.books, open, line, &share).await.unwrap();
    (repaid, partly, gear, open)
}

#[tokio::test]
async fn closed_splits_say_how_and_when_and_open_ones_stay_out() {
    let l = Ledger::new().await;
    let (repaid, partly, gear, _open) = history(&l).await;

    let closed = bill_split::closed(&l.pool, &ClosedFilter::default()).await.unwrap();
    let got: Vec<_> = closed.iter().map(|c| (c.split.id, c.how, c.on)).collect();
    assert_eq!(got, vec![
        (gear, Closing::WrittenOff, d(2026, 5, 1)),
        (partly, Closing::PartlyWrittenOff, d(2026, 5, 1)),
        (repaid, Closing::Repaid, d(2025, 3, 1)),
    ]);
}

#[tokio::test]
async fn closed_splits_filter_by_person_group_and_year() {
    let l = Ledger::new().await;
    let (repaid, partly, gear, _open) = history(&l).await;

    let person = ClosedFilter { person: Some(l.alice), ..Default::default() };
    assert_eq!(closed_ids(&l, &person).await, vec![gear, repaid]);
    let group = ClosedFilter { group: Some("Assets:Split:Gear".into()), ..Default::default() };
    assert_eq!(closed_ids(&l, &group).await, vec![gear]);
    let year = ClosedFilter { year: Some(2025), ..Default::default() };
    assert_eq!(closed_ids(&l, &year).await, vec![repaid]);
    let both = ClosedFilter { person: Some(l.bob), year: Some(2026), ..Default::default() };
    assert_eq!(closed_ids(&l, &both).await, vec![partly]);
}

#[tokio::test]
async fn a_netted_split_closes_when_its_balance_reaches_zero() {
    let l = Ledger::new().await;
    let split = l.netted("社團", CREW).await;
    let out = l.line("2026-01-10", &[(BANK, "-300"), (CREW, "300")]).await;
    bill_split::attach(&l.pool, &l.books, split, out, &Attach::Share(None)).await.unwrap();
    assert!(closed_ids(&l, &ClosedFilter::default()).await.is_empty());

    let back = l.line("2026-02-01", &[(BANK, "300"), (CREW, "-300")]).await;
    bill_split::gather(&l.pool, &l.books, split, &[back]).await.unwrap();
    assert_eq!(closed_ids(&l, &ClosedFilter::default()).await, vec![split]);
}
