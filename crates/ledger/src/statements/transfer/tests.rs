use chrono::NaiveDate;
use ledger_types::currency::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::{Half, MAX_DAYS_APART};
use crate::statements::bank::{Bank, BankStatement, Period, StatementLine};

fn day(d: u32) -> NaiveDate { NaiveDate::from_ymd_opt(2026, 5, d).expect("date") }

/// A download for `account_no`: `(day, delta)` lines from a zero balance.
fn statement(account_no: &str, lines: &[(u32, Decimal)]) -> BankStatement {
    let mut balance = Decimal::ZERO;
    BankStatement {
        bank: Bank::Cathay,
        account_no: account_no.into(),
        account_kind: String::new(),
        currency: Currency::TWD,
        period_end: None,
        periods: Vec::new(),
        lines: lines
            .iter()
            .map(|&(d, delta)| {
                balance += delta;
                StatementLine {
                    purchased: None,
                    book_date: day(d),
                    description: String::new(),
                    withdrawal: -delta.min(Decimal::ZERO),
                    deposit: delta.max(Decimal::ZERO),
                    balance,
                    info: String::new(),
                    memo: String::new(),
                }
            })
            .collect(),
    }
}

/// Whether line `si` of `send` pairs with line `ri` of `receive`, and the day
/// the pair is booked on.
fn pair(send: &BankStatement, si: usize, receive: &BankStatement, ri: usize) -> Option<NaiveDate> {
    let send = Half { statement: send, line: &send.lines[si] };
    let receive = Half { statement: receive, line: &receive.lines[ri] };
    send.pairs_with(&receive).then(|| send.booked_with(&receive))
}

#[test]
fn halves_a_weekend_apart_pair_on_the_earlier_day() {
    let send = statement("1", &[(1, dec!(900)), (4, dec!(-500))]);
    let receive = statement("2", &[(1, dec!(10)), (2, dec!(500))]);
    assert_eq!(pair(&send, 1, &receive, 1), Some(day(2)));
    let receive = statement("2", &[(1, dec!(10)), (6, dec!(500))]);
    assert_eq!(pair(&send, 1, &receive, 1), Some(day(4)));
}

/// A download states the balance it opens on, the day before its first line.
#[test]
fn a_download_opening_between_the_halves_keeps_them_apart() {
    let send = statement("1", &[(4, dec!(-500))]);
    let receive = statement("2", &[(1, dec!(10)), (2, dec!(500))]);
    assert_eq!(pair(&send, 0, &receive, 1), None);
}

#[test]
fn a_balance_stated_between_the_halves_keeps_them_apart() {
    // The sender's own line on the 3rd states its balance that day; moving
    // its half back to the 2nd would change it.
    let send = statement("1", &[(1, dec!(900)), (3, dec!(-20)), (4, dec!(-500))]);
    let receive = statement("2", &[(1, dec!(10)), (2, dec!(500))]);
    assert_eq!(pair(&send, 2, &receive, 1), None);
    // A line on the earlier day itself counts too.
    let send = statement("1", &[(1, dec!(900)), (2, dec!(-20)), (4, dec!(-500))]);
    assert_eq!(pair(&send, 2, &receive, 1), None);
}

#[test]
fn a_period_ending_between_the_halves_keeps_them_apart() {
    let mut send = statement("1", &[(4, dec!(-500))]);
    send.periods =
        vec![Period { start: day(1), end: day(2), opening: dec!(500), closing: dec!(500) }];
    let receive = statement("2", &[(1, dec!(500))]);
    assert_eq!(pair(&send, 0, &receive, 0), None);
}

#[test]
fn only_the_opposite_amount_in_another_account_near_enough_pairs() {
    let send = statement("1", &[(1, dec!(-500))]);
    assert_eq!(pair(&send, 0, &statement("2", &[(1, dec!(400))]), 0), None);
    assert_eq!(pair(&send, 0, &statement("1", &[(1, dec!(500))]), 0), None);
    let far = u32::try_from(1 + MAX_DAYS_APART + 1).expect("a day of the month");
    assert_eq!(pair(&send, 0, &statement("2", &[(far, dec!(500))]), 0), None);
}
