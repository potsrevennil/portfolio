use db::bill_split::{self, Closing, Status, WriteOff, BAD_DEBT};
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::fixture::*;

fn off(account: &str, amount: Option<Decimal>) -> WriteOff {
    WriteOff {
        account: account.into(),
        currency: Currency::TWD,
        date: d(2026, 6, 1),
        amount,
        note: Some("討無".into()),
    }
}

#[tokio::test]
async fn a_write_off_is_its_own_record_on_bad_debt() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-500"), (FOOD, "200"), (ALICE, "300")]).await;
    let legs = l.legs(line).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();

    let id = bill_split::write_off(&l.pool, &l.books, split, &off(ALICE, None)).await.unwrap();

    // The charge is untouched; the loss is a new record, created with its account.
    assert_eq!(l.legs(line).await, legs);
    assert_eq!(l.legs(id).await, dec_map(&[(ALICE, dec!(-300)), (BAD_DEBT, dec!(300))]));
    assert_eq!(l.balance(ALICE).await, dec!(0));
    assert!(l.reviewed(id).await);
    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.status(), Status::Settled);
    assert_eq!(split.closing().unwrap().0, Closing::WrittenOff);
}

#[tokio::test]
async fn part_can_be_written_off_and_no_more_than_is_owed() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-500"), (FOOD, "200"), (ALICE, "300")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();

    assert!(bill_split::write_off(&l.pool, &l.books, split, &off(ALICE, Some(dec!(301))))
        .await
        .is_err());
    assert!(bill_split::write_off(&l.pool, &l.books, split, &off(ALICE, Some(dec!(-1))))
        .await
        .is_err());
    bill_split::write_off(&l.pool, &l.books, split, &off(ALICE, Some(dec!(100)))).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.status(), Status::Partial);
    assert_eq!(split.owed()[&(l.alice, Currency::TWD)], dec!(200));
}

#[tokio::test]
async fn what_you_owe_cannot_be_written_off() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(FOOD, "500"), (ALICE, "-500")]).await;
    let split = l.per_person("她先付").await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();
    assert!(bill_split::write_off(&l.pool, &l.books, split, &off(ALICE, None)).await.is_err());
}

#[tokio::test]
async fn a_netted_write_off_takes_the_groups_balance() {
    let l = Ledger::new().await;
    let split = l.netted("社團", CREW).await;
    let line = l.line("2026-01-10", &[(BANK, "-300"), (CREW, "300")]).await;
    bill_split::attach(&l.pool, &l.books, split, line, &bill_split::Attach::Share(None))
        .await
        .unwrap();

    assert!(bill_split::write_off(&l.pool, &l.books, split, &off(ALICE, None)).await.is_err());
    bill_split::write_off(&l.pool, &l.books, split, &off(CREW, None)).await.unwrap();
    assert_eq!(l.balance(CREW).await, dec!(0));
}
