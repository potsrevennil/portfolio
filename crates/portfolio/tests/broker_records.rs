//! T4b acceptance on the user's own statements, which are gitignored and live
//! in the main checkout (or `$LEDGER_RECORDS_DIR`). Skipped where absent, as on
//! CI. Replaying each broker's parsed records must reproduce every balance its
//! statements state.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use portfolio::{
    broker::{cathay, firstrade, ib, replay, BrokerRecord, BrokerStatement, Commodity},
    securities::Securities,
};
use rust_decimal::Decimal;

fn records_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("LEDGER_RECORDS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .ok()?;
            PathBuf::from(String::from_utf8(out.stdout).ok()?.trim()).parent()?.to_path_buf()
        }
    };
    dir.join("raw/ib").is_dir().then_some(dir)
}

/// `$LEDGER_SECURITIES`, else the records' own securities.toml.
fn securities(records: &Path) -> Result<Securities> {
    let path = std::env::var_os("LEDGER_SECURITIES")
        .map_or_else(|| records.join("securities.toml"), PathBuf::from);
    Securities::load(&path.to_string_lossy())
}

fn files(dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| Ok(e?.path()))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == extension))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Every stated balance the replay misses, as readable lines.
fn misses(statements: &[BrokerStatement]) -> Vec<String> {
    let first = statements.iter().min_by_key(|s| s.period_start).expect("a statement");
    let openings = first.opening_records();
    let mut unique: BTreeMap<&str, &BrokerRecord> = BTreeMap::new();
    for r in statements.iter().flat_map(|s| &s.records).chain(&openings) {
        unique.insert(&r.key, r);
    }
    let mut out = Vec::new();
    for s in statements {
        let points = [
            s.opening.as_ref().map(|h| (s.period_start.pred_opt().unwrap(), h)),
            s.closing.as_ref().map(|h| (s.period_end, h)),
        ];
        for (as_of, holdings) in points.into_iter().flatten() {
            let computed = replay(unique.values().copied(), as_of);
            let stated: BTreeMap<Commodity, Decimal> = holdings.iter().collect();
            for c in computed.keys().chain(stated.keys()) {
                let (want, got) = (
                    stated.get(c).copied().unwrap_or_default(),
                    computed.get(c).copied().unwrap_or_default(),
                );
                // A listed position is complete; cash only for the currencies listed.
                let listed = stated.contains_key(c) || matches!(c, Commodity::Security(_));
                if listed && want != got {
                    out.push(format!("{as_of} {c}: stated {want}, replay {got}"));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn ib_statements_replay_to_their_own_balances() -> Result<()> {
    let Some(records) = records_dir() else { return Ok(()) };
    let statements = files(&records.join("raw/ib"), "csv")?
        .iter()
        .map(|p| ib::parse(&std::fs::read_to_string(p)?))
        .collect::<Result<Vec<_>>>()?;
    let misses = misses(&statements);
    assert!(misses.is_empty(), "{}", misses.join("\n"));
    Ok(())
}

#[test]
fn firstrade_statements_replay_to_their_own_balances() -> Result<()> {
    let Some(records) = records_dir() else { return Ok(()) };
    let texts = files(&records.join("raw/firstrade"), "pdf")?
        .iter()
        .map(|p| firstrade::pdf_text(p))
        .collect::<Result<Vec<_>>>()?;
    let statements = firstrade::parse_all(&texts, &securities(&records)?.symbols)?;
    let misses = misses(&statements);
    assert!(misses.is_empty(), "{}", misses.join("\n"));
    Ok(())
}

/// A line reduced to what both sides state: broker, settle date, symbol,
/// signed share change and cash change.
type Line = (String, String, String, Decimal, Decimal);

fn line(broker: &str, date: &str, symbol: &str, quantity: Decimal, cash: Decimal) -> Line {
    (
        broker.to_string(),
        date.to_string(),
        symbol.to_string(),
        quantity.normalize(),
        cash.normalize(),
    )
}

/// corrected/investments.csv in the same terms. Its stock deposits carry the
/// value in `amount` as cost basis, not as cash.
fn recorded(records: &Path, parties: &[&str]) -> Result<Vec<(Line, String)>> {
    let mut out = Vec::new();
    for row in csv::Reader::from_path(records.join("corrected/investments.csv"))?.deserialize() {
        let r: BTreeMap<String, String> = row?;
        let party = r["party"].as_str();
        if !parties.contains(&party) {
            continue;
        }
        let (kind, stocks) = (r["kind"].as_str(), r["asset_class"] == "Stocks");
        let decimal = |k: &str| r[k].parse::<Decimal>();
        let quantity = match (stocks, kind) {
            (true, "Sell" | "Withdrawal") => -decimal("quantity")?,
            _ => decimal("quantity")?,
        };
        let cash = match (stocks, kind) {
            (true, "Deposit" | "Withdrawal" | "CorporateAction") => Decimal::ZERO,
            _ => decimal("amount")? + decimal("commission")?,
        };
        // Firstrade statements date lines by settlement; the others by trade.
        let date = if party == "firstrade" { &r["settle_date"][..] } else { &r["date"][..10] };
        let symbol = if quantity.is_zero() && kind == "Deposit" { "" } else { &r["symbol"][..] };
        out.push((line(party, date, symbol, quantity, cash), format!("{kind} {}", r["note"])));
    }
    Ok(out)
}

/// Multiset difference: what `a` has that `b` lacks.
fn missing<'a>(a: &'a [(Line, String)], b: &[(Line, String)]) -> Vec<&'a (Line, String)> {
    let mut pool: BTreeMap<&Line, usize> = BTreeMap::new();
    for (l, _) in b {
        *pool.entry(l).or_default() += 1;
    }
    a.iter()
        .filter(|(l, _)| match pool.get_mut(l) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

/// The parsed records agree with corrected/investments.csv line for line. The
/// statements hold lines the records leave out (internal sweeps, vesting
/// notices), and the opening is dated by the statement period: those are
/// listed, not failures.
#[test]
fn parsed_statements_agree_with_the_corrected_records() -> Result<()> {
    let Some(records) = records_dir() else { return Ok(()) };
    let names = securities(&records)?.symbols;
    let mut statements: Vec<(&str, Vec<BrokerStatement>)> = vec![(
        "ib",
        files(&records.join("raw/ib"), "csv")?
            .iter()
            .map(|p| ib::parse(&std::fs::read_to_string(p)?))
            .collect::<Result<_>>()?,
    )];
    let texts = files(&records.join("raw/firstrade"), "pdf")?
        .iter()
        .map(|p| firstrade::pdf_text(p))
        .collect::<Result<Vec<_>>>()?;
    statements.push(("firstrade", firstrade::parse_all(&texts, &names)?));

    let mut parsed = Vec::new();
    for (party, list) in &statements {
        let first = list.iter().min_by_key(|s| s.period_start).expect("a statement");
        let mut unique: BTreeMap<String, BrokerRecord> = BTreeMap::new();
        for r in list.iter().flat_map(|s| s.records.iter()).chain(&first.opening_records()) {
            unique.insert(r.key.clone(), r.clone());
        }
        for r in unique.into_values() {
            let symbol = r.symbol.clone().unwrap_or_default();
            let l = line(party, &r.settle_date.to_string(), &symbol, r.quantity, r.cash());
            parsed.push((l, r.kind.to_string()));
        }
    }
    let recorded = recorded(&records, &["ib", "firstrade"])?;

    // Kept by the statements, left out of the records; set aside first so they
    // can't pair with a same-sized real line.
    let (notices, parsed): (Vec<_>, Vec<_>) = parsed
        .into_iter()
        .partition(|(_, kind)| matches!(kind.as_str(), "internal" | "award-vesting"));
    let extras = missing(&parsed, &recorded);
    let absent = missing(&recorded, &parsed);
    let unexplained: Vec<String> = extras
        .iter()
        .filter(|(_, kind)| kind != "opening")
        .map(|(l, kind)| format!("statement only: {kind} {l:?}"))
        .chain(
            absent
                .iter()
                // The opening deposit, dated by hand rather than by statement.
                .filter(|(l, note)| {
                    !(note.starts_with("Deposit") && extras.iter().any(|(e, k)| k == "opening" && e.4 == l.4))
                })
                .map(|(l, note)| format!("records only: {l:?} {note}")),
        )
        .collect();
    eprintln!(
        "{} parsed, {} recorded, {} internal or vesting lines only the statements keep",
        parsed.len(),
        recorded.len(),
        notices.len()
    );
    assert!(unexplained.is_empty(), "{}", unexplained.join("\n"));
    Ok(())
}

/// Every Cathay securities trade in the records is in the exports. Trades the
/// records don't hold yet are listed, not failed: the records end where they
/// were last reconciled.
#[test]
fn cathay_trades_in_the_records_are_all_in_the_exports() -> Result<()> {
    let Some(records) = records_dir() else { return Ok(()) };
    let names = securities(&records)?.symbols;
    let mut unique: BTreeMap<String, BrokerRecord> = BTreeMap::new();
    for p in files(&records.join("raw/cathay-securities"), "csv")? {
        for r in cathay::parse(&std::fs::read_to_string(p)?, &names)?.records {
            unique.insert(r.key.clone(), r);
        }
    }
    let parsed: Vec<(Line, String)> = unique
        .values()
        .map(|r| {
            let symbol = r.symbol.clone().unwrap_or_default();
            (
                line(
                    "cathay-securities",
                    &r.settle_date.to_string(),
                    &symbol,
                    r.quantity,
                    r.cash(),
                ),
                r.kind.to_string(),
            )
        })
        .collect();
    let trades: Vec<(Line, String)> = recorded(&records, &["cathay-securities"])?
        .into_iter()
        .filter(|(_, note)| note.starts_with("Buy") || note.starts_with("Sell"))
        .collect();
    let absent = missing(&trades, &parsed);
    let extra = missing(&parsed, &trades);
    let last = trades.iter().map(|(l, _)| l.1.clone()).max().unwrap_or_default();
    eprintln!(
        "{} exported trades, {} recorded; {} not in the records (newest recorded {last})",
        parsed.len(),
        trades.len(),
        extra.len()
    );
    assert!(absent.is_empty(), "{absent:?}");
    assert!(extra.iter().all(|(l, _)| l.1 > last), "a gap inside the recorded range: {extra:?}");
    Ok(())
}
