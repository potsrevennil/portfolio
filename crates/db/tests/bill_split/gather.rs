use db::bill_split::{self, Role, Status};
use ledger_types::currency::Currency;
use rust_decimal_macros::dec;

use crate::fixture::*;

#[tokio::test]
async fn gathering_groups_history_and_touches_no_balance() {
    let l = Ledger::new().await;
    let dinner = l.line("2026-01-10", &[(BANK, "-1000"), (FOOD, "500"), (ALICE, "500")]).await;
    let repaid = l.line("2026-02-10", &[(BANK, "200"), (ALICE, "-200")]).await;
    let before = l.balances().await;
    let legs = l.legs(dinner).await;

    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[repaid, dinner]).await.unwrap();

    assert_eq!(l.balances().await, before);
    assert_eq!(l.legs(dinner).await, legs);
    let split = bill_split::get(&l.pool, split).await.unwrap();
    let roles: Vec<(Role, _)> = split.shares.iter().map(|s| (s.role, s.amount)).collect();
    assert_eq!(roles, vec![(Role::Share, dec!(500)), (Role::Repayment, dec!(-200))]);
    assert_eq!(split.status(), Status::Partial);
    assert_eq!(split.owed()[&(l.alice, Currency::TWD)], dec!(300));
    assert_eq!(l.events(dinner).await, vec!["attached"]);
}

#[tokio::test]
async fn a_repayment_older_than_its_share_still_settles_it() {
    let l = Ledger::new().await;
    let early = l.line("2025-03-01", &[(BANK, "500"), (ALICE, "-500")]).await;
    let dinner = l.line("2026-01-10", &[(BANK, "-700"), (FOOD, "200"), (ALICE, "500")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[early, dinner]).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.status(), Status::Settled);
    assert_eq!(split.shares.iter().find(|s| s.role == Role::Repayment).unwrap().amount, dec!(-500));
}

#[tokio::test]
async fn money_paid_out_for_someone_is_their_share_until_something_opposes_it() {
    let l = Ledger::new().await;
    let lent = l.line("2026-01-10", &[(BANK, "-500"), (ALICE, "500")]).await;
    let back = l.line("2026-03-01", &[(BANK, "500"), (ALICE, "-500")]).await;
    let split = l.per_person("代墊").await;
    bill_split::gather(&l.pool, &l.books, split, &[lent, back]).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    let roles: Vec<Role> = split.shares.iter().map(|s| s.role).collect();
    assert_eq!(roles, vec![Role::Share, Role::Repayment]);
    assert_eq!(split.status(), Status::Settled);
}

#[tokio::test]
async fn a_friend_who_fronted_has_a_negative_share() {
    let l = Ledger::new().await;
    let fronted = l.line("2026-01-10", &[(FOOD, "500"), (ALICE, "-500")]).await;
    let paid = l.line("2026-03-01", &[(BANK, "-500"), (ALICE, "500")]).await;

    let split = l.per_person("她先付").await;
    bill_split::gather(&l.pool, &l.books, split, &[fronted, paid]).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.shares[0].role, Role::Share);
    assert_eq!(split.shares[1].role, Role::Repayment);
    assert_eq!(split.status(), Status::Settled);
}

#[tokio::test]
async fn your_own_cash_share_joins_as_yours() {
    let l = Ledger::new().await;
    let own = l.line("2026-01-10", &[(FOOD, "300"), (CASH, "-300")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[own]).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.shares[0].role, Role::Own);
    assert_eq!(split.shares[0].person, None);
    // Nobody owes anything, and it is not settled for that.
    assert_eq!(split.status(), Status::Open);
}

#[tokio::test]
async fn gathering_is_all_or_nothing() {
    let l = Ledger::new().await;
    let good = l.line("2026-01-10", &[(BANK, "-100"), (FOOD, "50"), (ALICE, "50")]).await;
    let unassigned = l.line("2026-01-11", &[(BANK, "-100"), (FOOD, "50"), (STRANGER, "50")]).await;
    let split = l.per_person("晚餐").await;

    let refused =
        bill_split::gather(&l.pool, &l.books, split, &[good, unassigned]).await.unwrap_err();
    assert!(refused.to_string().contains("指定"), "{refused}");
    assert!(bill_split::get(&l.pool, split).await.unwrap().shares.is_empty());
    assert!(l.events(good).await.is_empty());
}

#[tokio::test]
async fn an_uncategorized_line_is_refused_until_it_is_split() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-100"), (UNCAT_OUT, "100")]).await;
    let split = l.per_person("晚餐").await;
    assert!(bill_split::gather(&l.pool, &l.books, split, &[line]).await.is_err());
}

#[tokio::test]
async fn a_default_category_a_record_may_replace_is_no_share_of_yours() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-15"), (FEES, "15")]).await;
    let split = l.per_person("晚餐").await;
    assert!(bill_split::gather(&l.pool, &l.books, split, &[line]).await.is_err());
}

#[tokio::test]
async fn gathering_a_line_twice_is_refused() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-10", &[(BANK, "-100"), (FOOD, "50"), (ALICE, "50")]).await;
    let split = l.per_person("晚餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[line, line]).await.unwrap();
    assert_eq!(bill_split::get(&l.pool, split).await.unwrap().shares.len(), 1);

    let refused = bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap_err();
    assert!(refused.to_string().contains("已經在"), "{refused}");
}

#[tokio::test]
async fn a_netted_split_takes_only_its_own_account_and_is_balance_only() {
    let l = Ledger::new().await;
    let split = l.netted("社團", CREW).await;
    let line = l.line("2026-01-10", &[(BANK, "-300"), (CREW, "200"), (ALICE, "100")]).await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();

    let split = bill_split::get(&l.pool, split).await.unwrap();
    assert_eq!(split.shares.len(), 1);
    assert_eq!(split.shares[0].account, CREW);
    assert_eq!(split.shares[0].person, None);
    assert_eq!(split.status(), Status::BalanceOnly);
    assert_eq!(split.balance[&Currency::TWD], dec!(200));
}
