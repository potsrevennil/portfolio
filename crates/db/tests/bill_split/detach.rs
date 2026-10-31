use db::{
    bill_split::{self, Attach, Repayment, Status, WriteOff},
    review::Edit,
};
use ledger_types::currency::Currency;
use rust_decimal_macros::dec;

use crate::fixture::*;

async fn booked(l: &Ledger, split: i64) -> i64 {
    let line = l.line("2026-01-10", &[(BANK, "-1000"), (UNCAT_OUT, "1000")]).await;
    let ids = l.posting_ids(line).await;
    let edit = Edit {
        narration: None,
        legs: vec![
            leg(Some(ids[0]), BANK, dec!(-1000)),
            leg(Some(ids[1]), FOOD, dec!(500)),
            leg(None, ALICE, dec!(500)),
        ],
        confirm: true,
    };
    bill_split::attach(&l.pool, &l.books, split, line, &Attach::Share(Some(edit))).await.unwrap();
    line
}

#[tokio::test]
async fn detaching_a_line_the_engine_split_returns_it_to_the_queue_uncategorized() {
    let l = Ledger::new().await;
    let split = l.per_person("晚餐").await;
    let line = booked(&l, split).await;
    assert!(l.reviewed(line).await);

    bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();

    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(-1000)), (UNCAT_OUT, dec!(1000))]));
    assert!(!l.reviewed(line).await);
    assert!(bill_split::get(&l.pool, split).await.unwrap().shares.is_empty());
    let events = l.events(line).await;
    assert_eq!(&events[events.len() - 3..], ["detached", "edited", "unconfirmed"]);
}

#[tokio::test]
async fn detaching_a_gathered_line_keeps_its_legs_and_every_balance() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-1000"), (FOOD, "500"), (ALICE, "500")]).await;
    let before = l.balances().await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();

    bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();

    assert_eq!(l.balances().await, before);
    assert!(l.reviewed(line).await);
    assert_eq!(l.events(line).await, vec!["attached", "detached"]);
}

#[tokio::test]
async fn detaching_a_repayment_moved_off_uncategorized_returns_it_there() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-600"), (FOOD, "100"), (ALICE, "500")]).await;
    let line = l.line("2026-02-01", &[(BANK, "500"), (UNCAT_IN, "-500")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[dinner]).await.unwrap();
    let how = Attach::Repayment(Repayment { account: Some(ALICE.into()), amount: None });
    bill_split::attach(&l.pool, &l.books, split, line, &how).await.unwrap();

    bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();

    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(500)), (UNCAT_IN, dec!(-500))]));
    assert_eq!(bill_split::get(&l.pool, split).await.unwrap().status(), Status::Open);
}

#[tokio::test]
async fn detaching_a_repayment_returns_only_the_leg_it_moved() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-300"), (ALICE, "300")]).await;
    let line = l
        .line("2026-02-01", &[(BANK, "600"), (UNCAT_IN, "-300"), (BOB, "-200"), (FOOD, "-100")])
        .await;
    let split = l.per_person("晚餐").await;
    bill_split::attach(&l.pool, &l.books, split, dinner, &Attach::Share(None)).await.unwrap();
    let how = Attach::Repayment(Repayment { account: Some(ALICE.into()), amount: None });
    bill_split::attach(&l.pool, &l.books, split, line, &how).await.unwrap();

    bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();

    // Bob's balance and the category were never the engine's to touch.
    assert_eq!(
        l.legs(line).await,
        dec_map(&[
            (BANK, dec!(600)),
            (UNCAT_IN, dec!(-300)),
            (BOB, dec!(-200)),
            (FOOD, dec!(-100))
        ])
    );
}

#[tokio::test]
async fn the_last_split_to_let_go_returns_a_shared_repayment() {
    let l = Ledger::new().await;
    let one = l.line("2026-01-10", &[(BANK, "-300"), (ALICE, "300")]).await;
    let two = l.line("2026-01-20", &[(BANK, "-200"), (ALICE, "200")]).await;
    let line = l.line("2026-02-01", &[(BANK, "500"), (UNCAT_IN, "-500")]).await;
    let (first, second) = (l.per_person("一").await, l.per_person("二").await);
    bill_split::attach(&l.pool, &l.books, first, one, &Attach::Share(None)).await.unwrap();
    bill_split::attach(&l.pool, &l.books, second, two, &Attach::Share(None)).await.unwrap();
    let to_alice = Attach::Repayment(Repayment { account: Some(ALICE.into()), amount: None });
    bill_split::attach(&l.pool, &l.books, first, line, &to_alice).await.unwrap();
    bill_split::attach(&l.pool, &l.books, second, line, &to_alice).await.unwrap();

    bill_split::detach(&l.pool, &l.books, first, line).await.unwrap();
    // The second split still holds the leg, so it stays where it is.
    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(500)), (ALICE, dec!(-500))]));

    bill_split::detach(&l.pool, &l.books, second, line).await.unwrap();
    assert_eq!(l.legs(line).await, dec_map(&[(BANK, dec!(500)), (UNCAT_IN, dec!(-500))]));
}

#[tokio::test]
async fn a_leg_only_gathered_stays_in_whichever_order_the_splits_let_go() {
    for s1_first in [true, false] {
        let l = Ledger::new().await;
        let dinner = l.line("2026-01-10", &[(BANK, "-300"), (ALICE, "300")]).await;
        let line = l.line("2026-02-01", &[(BANK, "500"), (UNCAT_IN, "-300"), (BOB, "-200")]).await;
        let (s1, s2) = (l.per_person("一").await, l.per_person("二").await);
        bill_split::attach(&l.pool, &l.books, s1, dinner, &Attach::Share(None)).await.unwrap();
        let to_alice = Attach::Repayment(Repayment { account: Some(ALICE.into()), amount: None });
        bill_split::attach(&l.pool, &l.books, s1, line, &to_alice).await.unwrap();
        bill_split::gather(&l.pool, &l.books, s2, &[line]).await.unwrap();

        let order = if s1_first { [s1, s2] } else { [s2, s1] };
        for split in order {
            bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();
        }

        // Only Alice's leg was the engine's; Bob's was on his account all along.
        assert_eq!(
            l.legs(line).await,
            dec_map(&[(BANK, dec!(500)), (UNCAT_IN, dec!(-300)), (BOB, dec!(-200))]),
            "s1 first: {s1_first}"
        );
    }
}

#[tokio::test]
async fn a_friend_fronted_line_has_no_money_leg_to_return_and_keeps_its_legs() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(FOOD, "1000"), (UNCAT_IN, "-1000")]).await;
    let ids = l.posting_ids(line).await;
    let edit = Edit {
        narration: None,
        legs: vec![leg(Some(ids[0]), FOOD, dec!(1000)), leg(Some(ids[1]), ALICE, dec!(-1000))],
        confirm: false,
    };
    let split = l.per_person("她先付").await;
    bill_split::attach(&l.pool, &l.books, split, line, &Attach::Share(Some(edit))).await.unwrap();

    bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();

    assert_eq!(l.legs(line).await, dec_map(&[(FOOD, dec!(1000)), (ALICE, dec!(-1000))]));
}

#[tokio::test]
async fn deleting_a_split_detaches_everything_and_deletes_no_line() {
    let l = Ledger::new().await;
    let split = l.per_person("晚餐").await;
    let written = booked(&l, split).await;
    let gathered = l.line("2026-02-01", &[(BANK, "-100"), (FOOD, "50"), (ALICE, "50")]).await;
    bill_split::gather(&l.pool, &l.books, split, &[gathered]).await.unwrap();

    bill_split::delete(&l.pool, &l.books, split).await.unwrap();

    assert!(bill_split::get(&l.pool, split).await.is_err());
    assert!(bill_split::list(&l.pool).await.unwrap().is_empty());
    assert_eq!(l.legs(written).await, dec_map(&[(BANK, dec!(-1000)), (UNCAT_OUT, dec!(1000))]));
    assert_eq!(l.legs(gathered).await.len(), 3);
}

#[tokio::test]
async fn detaching_a_write_off_takes_it_back() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-500"), (FOOD, "200"), (ALICE, "300")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();
    let w = WriteOff {
        account: ALICE.into(),
        currency: Currency::TWD,
        date: d(2026, 6, 1),
        amount: None,
        note: None,
    };
    let off = bill_split::write_off(&l.pool, &l.books, split, &w).await.unwrap();
    assert_eq!(l.balance(ALICE).await, dec!(0));

    bill_split::detach(&l.pool, &l.books, split, off).await.unwrap();

    assert_eq!(l.balance(ALICE).await, dec!(300));
    assert_eq!(bill_split::get(&l.pool, split).await.unwrap().status(), Status::Open);
    assert_eq!(l.events(off).await, vec!["entered", "attached", "detached", "deleted"]);
}

#[tokio::test]
async fn a_line_not_in_the_split_cannot_be_detached() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-500"), (ALICE, "500")]).await;
    let split = l.per_person("晚餐").await;
    assert!(bill_split::detach(&l.pool, &l.books, split, line).await.is_err());
}
