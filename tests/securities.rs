//! The securities config, loaded before anything is imported.

use std::io::Write;

use chrono::NaiveDate;
use portfolio::securities::Securities;
use rust_decimal::Decimal;
use tempfile::NamedTempFile;

fn temp(contents: &str) -> anyhow::Result<NamedTempFile> {
    let mut file = NamedTempFile::new()?;
    file.write_all(contents.as_bytes())?;
    Ok(file)
}

/// The shipped example is the only documentation of the config format, and it
/// is what a new setup gets copied from. If the format changes and the example
/// is not updated, this fails rather than the user's first run.
#[test]
fn the_example_config_is_valid() -> anyhow::Result<()> {
    let securities = Securities::load("securities.example.toml")?;
    assert!(!securities.symbols.is_empty(), "example maps no security name");
    assert!(!securities.splits.is_empty(), "example lists no split");
    Ok(())
}

/// A split that does not say how large it was fails at load with the field
/// named, rather than deep inside the holdings calculation.
#[test]
fn a_split_missing_its_ratio_is_rejected() -> anyhow::Result<()> {
    let config = temp("[[splits]]\nsymbol = \"ZZ01.TW\"\ndate = \"2024-07-01\"\n")?;
    let error = Securities::load(config.path().to_str().unwrap())
        .expect_err("a split with no ratio should not load");
    assert!(
        format!("{error:#}").contains("ratio"),
        "error should name the missing field, got: {error:#}"
    );
    Ok(())
}

#[test]
fn a_zero_split_ratio_is_rejected() -> anyhow::Result<()> {
    let config = temp("[[splits]]\nsymbol = \"ZZ01.TW\"\ndate = \"2024-07-01\"\nratio = 0\n")?;
    let error = Securities::load(config.path().to_str().unwrap())
        .expect_err("a zero ratio should not load");
    assert!(
        format!("{error:#}").contains("splits[0].ratio"),
        "error should name the offending split, got: {error:#}"
    );
    Ok(())
}

/// The split store keys splits by date, so two on one day must both survive,
/// and a whole-number ratio must read the same as a fractional one.
#[test]
fn splits_on_the_same_day_are_kept_together() -> anyhow::Result<()> {
    let config = temp(concat!(
        "[[splits]]\nsymbol = \"ZZ01.TW\"\ndate = \"2024-07-01\"\nratio = 4\n",
        "[[splits]]\nsymbol = \"ZZ02.TW\"\ndate = \"2024-07-01\"\nratio = 0.5\n",
    ))?;
    let splits = Securities::load(config.path().to_str().unwrap())?.stock_splits();

    let day = NaiveDate::from_ymd_opt(2024, 7, 1).unwrap();
    assert_eq!(splits.len(), 1);
    assert_eq!(splits[&day], vec![
        ("ZZ01.TW".to_string(), Decimal::from(4)),
        ("ZZ02.TW".to_string(), Decimal::new(5, 1)),
    ]);
    Ok(())
}
