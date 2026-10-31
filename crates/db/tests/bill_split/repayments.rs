use db::bill_split::{self, Attach};
use ledger_types::currency::Currency;
use rust_decimal_macros::dec;

use crate::fixture::*;

#[tokio::test]
async fn open_shares_and_unallocated_repayments_span_a_persons_accounts() {
    let l = Ledger::new().await;
    let split = l.per_person("裝備").await;
    let trip = l.line("2026-01-10", &[(BANK, "-500"), (ALICE_GEAR, "500")]).await;
    bill_split::attach(&l.pool, &l.books, split, trip, &Attach::Share(None)).await.unwrap();
    let back = l.line("2026-03-01", &[(BANK, "200"), (ALICE, "-200")]).await;
    // Not a repayment: it is booked to a category.
    l.line("2026-03-02", &[(FOOD, "50"), (ALICE, "-50")]).await;

    let open = bill_split::open_for(&l.pool, l.alice).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!((open[0].split_id, open[0].owed), (split, dec!(500)));
    assert_eq!(open[0].since, d(2026, 1, 10));
    assert!(bill_split::open_for(&l.pool, l.bob).await.unwrap().is_empty());

    let waiting = bill_split::unallocated(&l.pool, &l.books, l.alice).await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].transaction_id, back);
    assert_eq!((waiting[0].left, waiting[0].currency), (dec!(-200), Currency::TWD));
}

#[tokio::test]
async fn a_debts_age_is_its_oldest_share_not_an_older_repayment() {
    let l = Ledger::new().await;
    let split = l.per_person("晚餐").await;
    let early = l.line("2025-01-01", &[(BANK, "100"), (ALICE, "-100")]).await;
    let dinner = l.line("2026-01-10", &[(BANK, "-300"), (FOOD, "100"), (ALICE, "200")]).await;
    bill_split::gather(&l.pool, &l.books, split, &[early, dinner]).await.unwrap();

    let open = bill_split::open_for(&l.pool, l.alice).await.unwrap();
    assert_eq!(open[0].since, d(2026, 1, 10));
}

#[tokio::test]
async fn a_repayment_stops_waiting_once_a_split_has_taken_all_of_it() {
    let l = Ledger::new().await;
    let split = l.per_person("晚餐").await;
    let dinner = l.line("2026-01-10", &[(BANK, "-300"), (FOOD, "100"), (ALICE, "200")]).await;
    bill_split::gather(&l.pool, &l.books, split, &[dinner]).await.unwrap();
    let back = l.line("2026-03-01", &[(BANK, "500"), (ALICE, "-500")]).await;
    bill_split::gather(&l.pool, &l.books, split, &[back]).await.unwrap();

    // 200 of it was owed here; 300 still has no share to go to.
    let waiting = bill_split::unallocated(&l.pool, &l.books, l.alice).await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].left, dec!(-300));
}
