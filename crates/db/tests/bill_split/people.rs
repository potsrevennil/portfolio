use db::bill_split::{self, Shape};
use ledger_types::currency::Currency;
use rust_decimal_macros::dec;

use crate::fixture::*;

#[tokio::test]
async fn a_persons_balance_sums_all_their_accounts() {
    let l = Ledger::new().await;
    l.line("2026-01-01", &[(BANK, "-500"), (ALICE, "500")]).await;
    l.line("2026-01-02", &[(BANK, "-200"), (ALICE_GEAR, "200")]).await;
    l.line("2026-01-03", &[(BANK, "-900"), (BOB, "900")]).await;

    let people = bill_split::people(&l.pool).await.unwrap();
    let alice = people.iter().find(|p| p.id == l.alice).unwrap();
    assert_eq!(alice.accounts, vec![ALICE.to_string(), ALICE_GEAR.to_string()]);
    assert_eq!(alice.balance[&Currency::TWD], dec!(700));
}

#[tokio::test]
async fn netted_and_per_person_never_mix_under_one_group() {
    let l = Ledger::new().await;
    // Alice holds Split:Gear:Alice, so Split:Gear cannot be netted.
    let gear = Shape::Netted { account: "Assets:Split:Gear".into() };
    let refused = bill_split::create(&l.pool, &l.books, "露營", &gear).await.unwrap_err();
    assert!(refused.to_string().contains("不可混用"), "{refused}");

    // And a netted group's account cannot then go to a person, nor its child.
    l.netted("社團", CREW).await;
    for path in [CREW.to_string(), format!("{CREW}:Alice")] {
        let refused = bill_split::assign(&l.pool, &l.books, l.alice, &path).await.unwrap_err();
        assert!(refused.to_string().contains("不可混用"), "{refused}");
    }

    // A second netted split on the same account is fine: the choice is the group's.
    l.netted("社團第二次", CREW).await;
}

#[tokio::test]
async fn the_ledger_itself_counts_as_using_a_shape() {
    let l = Ledger::new().await;
    sqlx::query("INSERT INTO accounts (path, label, type) VALUES (?, 'x', 'asset')")
        .bind("Assets:Split:Team:Dana")
        .execute(&l.pool)
        .await
        .unwrap();
    l.line("2026-01-01", &[(BANK, "-100"), ("Assets:Split:Team:Dana", "100")]).await;

    let team = Shape::Netted { account: "Assets:Split:Team".into() };
    assert!(bill_split::create(&l.pool, &l.books, "隊", &team).await.is_err());
}

#[tokio::test]
async fn only_accounts_under_a_split_root_can_be_someones() {
    let l = Ledger::new().await;
    for path in [BANK, "Assets:Split"] {
        assert!(bill_split::assign(&l.pool, &l.books, l.alice, path).await.is_err(), "{path}");
    }
    let already = bill_split::assign(&l.pool, &l.books, l.bob, ALICE).await.unwrap_err();
    assert!(already.to_string().contains("別人"), "{already}");
}

#[tokio::test]
async fn an_assignment_in_use_cannot_be_taken_back() {
    let l = Ledger::new().await;
    let line = l.line("2026-01-01", &[(BANK, "-100"), (BOB, "100")]).await;
    let split = l.per_person("午餐").await;
    bill_split::gather(&l.pool, &l.books, split, &[line]).await.unwrap();

    assert!(bill_split::unassign(&l.pool, l.bob, BOB).await.is_err());
    bill_split::detach(&l.pool, &l.books, split, line).await.unwrap();
    bill_split::unassign(&l.pool, l.bob, BOB).await.unwrap();
    assert!(bill_split::unassign(&l.pool, l.bob, BOB).await.is_err());
}

#[tokio::test]
async fn names_are_unique_and_can_change() {
    let l = Ledger::new().await;
    assert!(bill_split::add_person(&l.pool, "Alice").await.is_err());
    assert!(bill_split::add_person(&l.pool, "  ").await.is_err());
    assert!(bill_split::rename_person(&l.pool, l.bob, "Alice").await.is_err());
    bill_split::rename_person(&l.pool, l.bob, "Robert").await.unwrap();
    let people = bill_split::people(&l.pool).await.unwrap();
    assert!(people.iter().any(|p| p.id == l.bob && p.name == "Robert"));
}
