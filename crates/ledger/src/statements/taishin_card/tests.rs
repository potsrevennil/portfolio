use rust_decimal_macros::dec;

use super::{super::bank::Bank, *};

fn day(d: &str) -> NaiveDate { d.parse().unwrap() }

/// A statement's text, with `rows` in its table.
fn text(opening: &str, closing: &str, rows: &str) -> String {
    format!(
        "115 年 03 月 信用卡電子帳單\n帳單結帳日 115/03/09\n繳款截止日 115/03/28\n上期應繳總額 \
         {opening}\n-已繳退款總額 0\n=前期餘額 0\n+本期新增款項 0\n=本期累計應繳金額 \
         {closing}\n本期最低應繳金額 1\n下列消費明細未載明交易地區\n消費日 入帳起息日消費明細 \
         新臺幣金額 外幣折算日 消費地 幣別 外幣金額\n{rows}台端依契約得選擇\n消費日 入帳起息日 \
         分期交易尚未到期資訊\n115/01/01 115/01/02 其他 999 TW\n"
    )
}

fn bill(opening: &str, closing: &str, rows: &str) -> Result<Bill> {
    text(opening, closing, rows).parse()
}

fn only(bill: &Bill) -> &Line {
    let [line] = bill.lines.as_slice() else { panic!("{bill:?}") };
    line
}

#[test]
fn the_figures_and_a_plain_row_are_read() -> Result<()> {
    let b = bill("1,000", "1,250", "115/02/10 115/02/12 咖啡店TAIPEI 250 TW\n")?;
    assert_eq!((b.closes, b.opening, b.closing), (day("2026-03-09"), dec!(1000), dec!(1250)));
    assert_eq!(only(&b), &Line {
        purchased: Some(day("2026-02-10")),
        posted: day("2026-02-12"),
        description: "咖啡店TAIPEI".into(),
        amount: dec!(250),
        detail: String::new(),
    });
    Ok(())
}

#[test]
fn a_foreign_row_keeps_what_it_was_in() -> Result<()> {
    let b = bill("0", "303", "115/02/10 115/02/12 EXAMPLE9298 SHOP 303 0211 US USD 10.00\n")?;
    assert_eq!(only(&b).amount, dec!(303));
    assert_eq!(only(&b).description, "EXAMPLE9298 SHOP");
    assert_eq!(only(&b).detail, "0211 US USD 10.00");
    Ok(())
}

#[test]
fn a_wrapped_description_runs_up_to_the_line_with_the_amount() -> Result<()> {
    let b = bill("0", "530", "115/02/10 115/02/12\n範例商店_\n一號店?\n530 TW\n")?;
    assert_eq!(only(&b).description, "範例商店_一號店?");
    assert_eq!(only(&b).amount, dec!(530));
    Ok(())
}

#[test]
fn a_description_ending_in_a_number_is_not_the_amount() -> Result<()> {
    let b = bill("0", "530", "115/02/10 115/02/12 範例 21\n商店\n530 TW\n")?;
    assert_eq!(only(&b).amount, dec!(530));
    assert_eq!(only(&b).description, "範例 21商店");
    Ok(())
}

#[test]
fn a_fee_and_a_rebate_print_a_bare_amount() -> Result<()> {
    let rows = "115/02/10 115/02/12 國外交易服務費－650.00 10\n115/02/11 115/02/13 消費折抵_範例 \
                -30\n115/02/12 115/02/14 繳款 -1,000\n";
    let b = bill("1,000", "-20", rows)?;
    let amounts: Vec<Decimal> = b.lines.iter().map(|l| l.amount).collect();
    assert_eq!(amounts, [dec!(10), dec!(-30), dec!(-1000)]);
    assert_eq!(b.lines[0].description, "國外交易服務費－650.00");
    Ok(())
}

#[test]
fn a_page_break_can_join_rows_and_notices_can_follow_one() -> Result<()> {
    let rows = "115/02/10 115/02/12 甲店 52 TW\u{c}115/02/11 115/02/13 乙店 48 \
                TW\n◎重要通知，請詳閱。\nRichart卡(範例) (卡號末四碼:0000)\n115/02/12 115/02/14 \
                丙店 100 TW\u{c}*本行公告事項，詳見網站，\n";
    let b = bill("0", "200", rows)?;
    let seen: Vec<&str> = b.lines.iter().map(|l| l.description.as_str()).collect();
    assert_eq!(seen, ["甲店", "乙店", "丙店"]);
    Ok(())
}

#[test]
fn a_row_with_no_amount_is_refused() {
    assert!(bill("0", "0", "115/02/10 115/02/12 只有說明\n").is_err());
}

#[test]
fn a_statement_with_no_table_is_refused() {
    let no_table = "帳單結帳日 115/03/09\n上期應繳總額 0\n=本期累計應繳金額 0\n";
    assert!(no_table.parse::<Bill>().is_err());
}

#[test]
fn a_row_read_wrong_stops_the_statement_at_its_sum() -> Result<()> {
    let wrong = bill("0", "100", "115/02/10 115/02/12 甲店 90 TW\n")?;
    assert!(wrong.into_statement(Bank::TaishinCard, "c").is_err());
    Ok(())
}

#[test]
fn roc_dates_are_exact() {
    assert_eq!(roc_date("115/08/09"), Some(day("2026-08-09")));
    assert_eq!(roc_date("115/13/09"), None);
    assert_eq!(roc_date("115/8/9"), None);
    assert_eq!(roc_date("2026/08/09"), None);
}
