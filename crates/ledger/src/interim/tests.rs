use chrono::NaiveDate;
use rust_decimal_macros::dec;

use super::*;

fn chart() -> Chart {
    toml::from_str(
        r#"
        [institution]
        app_account = "銀行"
        primary = "Assets:Bank:Savings"
        settlement = "Assets:Bank:Investment"
        settlement_app_account = "證券"
        clearing = "Assets:Bank:Clearing"
        [institution.accounts]
        "111" = "Assets:Bank:Savings"
        "222" = "Assets:Bank:Investment"
        "333" = "Assets:Bank:Other"
        [fallback]
        income = "Income:Uncategorized"
        expense = "Expenses:Uncategorized"
        [expenses]
        "飲食" = { account = "Expenses:Food", tags = ["dining"] }
        [accounts]
        "現金" = "Assets:Cash"
        "別間" = "Assets:Bank:Other"
        "證券" = "Assets:Securities"
        "起鼓" = "Equity:Opening-Balances"
        "#,
    )
    .expect("test chart parses")
}

fn day() -> NaiveDate { NaiveDate::from_ymd_opt(2026, 10, 1).unwrap() }

fn flow(account: &str, id: &str) -> Entry {
    Entry::Flow {
        date: day(),
        account: account.into(),
        amount: dec!(-120),
        currency: Currency::TWD,
        category: "飲食".into(),
        memo: "午餐".into(),
        id: id.into(),
    }
}

fn transfer(from: &str, to: &str, id: &str) -> Entry {
    Entry::Transfer {
        date: day(),
        from: from.into(),
        out: dec!(500),
        out_currency: Currency::TWD,
        to: to.into(),
        inn: dec!(500),
        in_currency: Currency::TWD,
        memo: String::new(),
        id: id.into(),
    }
}

#[test]
fn a_cash_record_books_as_written_under_its_ref() -> Result<()> {
    let books = book(&chart(), &[flow("現金", "U1")])?;
    let [Booked { booking: Booking::Standalone(t), .. }] = books.records.as_slice() else {
        panic!("not standalone: {:?}", books.records)
    };
    assert_eq!(t.external_ref.as_deref(), Some("tiantian:U1"));
    let accounts: Vec<&str> = t.postings.iter().map(|p| p.account.as_str()).collect();
    assert_eq!(accounts, ["Assets:Cash", "Expenses:Food"]);
    assert_eq!(t.tags, ["dining"]);
    Ok(())
}

#[test]
fn a_bank_record_lands_on_the_primary_account_untagged() -> Result<()> {
    let books = book(&chart(), &[flow("銀行", "U2")])?;
    let [Booked { booking: Booking::OnStatement { leg, transaction }, .. }] =
        books.records.as_slice()
    else {
        panic!("not on a statement: {:?}", books.records)
    };
    assert_eq!(leg, &StatementLeg {
        account: "Assets:Bank:Savings".into(),
        amount: dec!(-120),
        currency: Currency::TWD,
    });
    assert!(!transaction.tags.iter().any(|t| t == "unverified"));
    assert_eq!(transaction.external_ref.as_deref(), Some("tiantian:U2"));
    Ok(())
}

#[test]
fn settlement_passes_through_the_settlement_account() -> Result<()> {
    let books = book(&chart(), &[transfer("銀行", "證券", "U3")])?;
    let [Booked { booking: Booking::OnStatement { leg, .. }, transfer: true, .. }] =
        books.records.as_slice()
    else {
        panic!("not on a statement: {:?}", books.records)
    };
    assert_eq!(leg.account, "Assets:Bank:Investment");
    assert_eq!(leg.amount, dec!(-500));
    Ok(())
}

#[test]
fn a_transfer_between_statement_accounts_is_left_to_them() -> Result<()> {
    let books = book(&chart(), &[transfer("銀行", "別間", "U4")])?;
    assert!(matches!(books.records.as_slice(), [Booked {
        booking: Booking::LeftToStatements,
        ..
    }]));
    Ok(())
}

#[test]
fn an_opening_or_a_record_without_uuid_is_refused() {
    assert!(book(&chart(), &[transfer("起鼓", "現金", "U5")]).is_err());
    assert!(book(&chart(), &[flow("現金", "")]).is_err());
}
