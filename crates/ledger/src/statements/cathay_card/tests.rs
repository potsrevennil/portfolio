use rust_decimal_macros::dec;

use super::*;

fn parsed(rows: &[&[&str]]) -> Result<Bill> {
    let rows: Vec<csv::StringRecord> =
        rows.iter().map(|r| csv::StringRecord::from(r.to_vec())).collect();
    Bill::try_from(rows.as_slice())
}

const HEAD: [&[&str]; 4] = [
    &["最低應繳總金額", "帳單結帳日"],
    &["TWD 1", "2026/01/18"],
    &["上期帳單總額", "繳款金額", "本期應繳總額"],
    &["TWD 1,000", "TWD 1,000", "TWD 200"],
];
const TABLE: &[&str] =
    &["消費日", "交易說明", "新臺幣金額", "消費國家/幣別", "消費金額", "入帳起息日"];

#[test]
fn a_late_december_purchase_on_a_january_statement_is_last_year() -> Result<()> {
    let bill = parsed(&[
        HEAD[0],
        HEAD[1],
        HEAD[2],
        HEAD[3],
        TABLE,
        &["−", "上期帳單總額", "1,000", "−", "−", "−"],
        &["12/30", "ABROAD", "200", "JP / JPY", "1,000", "01/02"],
    ])?;

    let [line] = bill.lines.as_slice() else { panic!("{bill:?}") };
    assert_eq!(line.purchased, "2025-12-30".parse().ok());
    assert_eq!(line.posted, "2026-01-02".parse()?);
    assert_eq!(line.amount, dec!(200));
    assert_eq!(line.detail, "JP / JPY 1,000");
    assert_eq!((bill.opening, bill.closing), (dec!(1000), dec!(200)));
    Ok(())
}

#[test]
fn a_waived_fee_at_zero_is_no_line() -> Result<()> {
    let bill = parsed(&[HEAD[0], HEAD[1], HEAD[2], HEAD[3], TABLE, &[
        "01/01",
        "年費優惠",
        "0",
        "−",
        "−",
        "01/01",
    ]])?;
    assert_eq!(bill.lines, vec![]);
    Ok(())
}

#[test]
fn a_statement_with_no_table_is_refused() {
    assert!(parsed(&[HEAD[0], HEAD[1], HEAD[2], HEAD[3]]).is_err());
}

#[test]
fn a_cell_it_cannot_read_is_refused() {
    let unreadable = parsed(&[HEAD[0], HEAD[1], HEAD[2], HEAD[3], TABLE, &[
        "01/01", "咖啡", "abc", "−", "−", "01/01",
    ]]);
    assert!(unreadable.is_err());
}
