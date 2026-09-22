//! The reporting engine: pure, read-only queries over the SQLite core schema.
//!
//! Everything here returns typed structs, never formatted strings, so the same
//! numbers can feed the Leptos UI, a CLI, or a test's assertions. IO and
//! arithmetic are kept apart: [`LedgerData::load`] reads the whole ledger once,
//! and the report methods on it are pure functions of that snapshot plus an
//! injected price table. The database-backed wrappers ([`account_balances`],
//! [`net_worth_over_time`], [`periodic_report`]) just wire the two together and
//! pull FX rates from the shared `stock_prices` table.
//!
//! Sign convention follows Beancount, as the bake emits it: assets and expenses
//! are debit-positive, liabilities/equity/income credit-negative. A balance is
//! therefore the plain signed sum of its postings; net worth is
//! `assets + liabilities` (liabilities already carry their own negative sign),
//! and equity — including the cross-currency `Equity:Conversions` plug the bake
//! inserts in place of an `@@` price — is deliberately excluded. So are
//! holdings carried at cost ([`AtCost`]); [`in_net_worth`] is the one rule.
//!
//! Amounts are stored as exact `TEXT` decimals; they are summed here with
//! `rust_decimal`, never with SQL `SUM` over a lossy `CAST` to `REAL`.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, Months, NaiveDate};
pub use ledger::accounts::in_subtree;
use ledger::valuation::AtCost;
use ledger_types::currency::Currency;
use portfolio::portfolio::{holding::Holding, statement::Statement, Portfolio};
use prices::{source::YFinanceSource, PriceStore, StockPrice};
use rust_decimal::{prelude::FromPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use strum_macros::{Display, EnumIter, EnumString};

// --- Report vocabulary ---------------------------------------------------

/// One of the five account roots, carrying the sign convention a report needs.
/// The string form is the schema's `accounts.type` CHECK vocabulary; strum
/// generates the parse/display so the DB round-trip needs no hand-written
/// match.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, EnumIter,
)]
#[strum(serialize_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AccountType {
    Asset,
    Liability,
    Equity,
    Income,
    Expense,
}

/// The schema word for each Beancount root.
impl From<ledger::accounts::AccountType> for AccountType {
    fn from(root: ledger::accounts::AccountType) -> Self {
        use ledger::accounts::AccountType as Root;
        match root {
            Root::Assets => Self::Asset,
            Root::Liabilities => Self::Liability,
            Root::Equity => Self::Equity,
            Root::Income => Self::Income,
            Root::Expenses => Self::Expense,
        }
    }
}

/// The calendar granularity a report is grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, EnumIter, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Grain {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// The balance of a single account in a single currency, as of a date.
///
/// Both `path` (the ASCII chart path) and `label` (the display label the loader
/// currently derives from the ASCII leaf) are returned, so the UI has a name to
/// show without a second lookup and does not depend on how the label is
/// sourced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountBalance {
    pub account_id: i64,
    pub path: String,
    pub label: String,
    pub account_type: AccountType,
    pub closed: bool,
    pub currency: Currency,
    pub amount: Decimal,
    pub as_of: NaiveDate,
}

/// Total assets and liabilities at one instant, converted to a base currency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetWorthPoint {
    pub date: NaiveDate,
    /// Signed sum of asset-account balances, in `base`.
    pub assets: Decimal,
    /// Signed sum of liability-account balances, in `base` (normally ≤ 0).
    pub liabilities: Decimal,
    /// `assets + liabilities`.
    pub net: Decimal,
}

/// Net worth sampled at the end of each period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetWorthSeries {
    pub base: Currency,
    pub grain: Grain,
    pub points: Vec<NetWorthPoint>,
}

/// Income and expense flow within one period, converted to a base currency.
/// Both are reported as positive magnitudes — money in and money out — so `net`
/// (savings) is `income - expense`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Period {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub income: Decimal,
    pub expense: Decimal,
    pub net: Decimal,
}

/// Income/expense flow grouped by period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeriodicReport {
    pub base: Currency,
    pub grain: Grain,
    pub periods: Vec<Period>,
}

/// One security position, projected from the `portfolio` module's `Holding`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoldingPosition {
    pub symbol: String,
    pub description: String,
    pub currency: Currency,
    pub quantity: Decimal,
    pub total_cost: Decimal,
    pub average_cost: Decimal,
    pub market_price: Decimal,
    pub market_value: Decimal,
    pub unrealized_pnl_value: Decimal,
    pub unrealized_pnl_percentage: Decimal,
    pub realized_pnl_value: Decimal,
}

/// Holdings as of a date, with the totals the `portfolio` module already rolls
/// up. Values are in `reporting_currency`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoldingsReport {
    pub as_of: NaiveDate,
    pub reporting_currency: Currency,
    pub positions: Vec<HoldingPosition>,
    pub total_cost: Decimal,
    pub total_market_value: Decimal,
    pub total_unrealized_pnl: Decimal,
    pub total_realized_pnl: Decimal,
    pub total_cash: Decimal,
    pub total_value: Decimal,
}

// --- The in-memory snapshot ----------------------------------------------

/// The chart-of-accounts metadata every report row is decorated with.
#[derive(Debug, Clone)]
struct AccountMeta {
    path: String,
    label: String,
    account_type: AccountType,
    closed: bool,
}

/// One posting leg, with its transaction date denormalised in for date
/// filtering without a re-join.
#[derive(Debug, Clone)]
struct PostingEntry {
    account_id: i64,
    date: NaiveDate,
    currency: Currency,
    amount: Decimal,
}

/// The whole ledger, read once and reported over many times in memory. A
/// single-user ledger is small enough that loading it whole beats issuing one
/// query per report date.
#[derive(Debug, Clone, Default)]
pub struct LedgerData {
    accounts: BTreeMap<i64, AccountMeta>,
    postings: Vec<PostingEntry>,
}

// Column-mirror rows for `query_as`; money and dates stay as strings until the
// typed parsing below.
#[derive(sqlx::FromRow)]
struct AccountRow {
    id: i64,
    path: String,
    label: String,
    acct_type: String,
    closed: i64,
}

#[derive(sqlx::FromRow)]
struct PostingRow {
    account_id: i64,
    date: String,
    currency: String,
    amount: String,
}

/// Parse a text column through its standard `FromStr`, naming the field so a
/// bad value fails loudly. One helper covers every column type: `Currency` and
/// `AccountType` (strum), `Decimal` (rust_decimal), `NaiveDate` (chrono).
fn field<T>(name: &str, s: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    s.parse().with_context(|| format!("{name} {s:?} is invalid"))
}

impl TryFrom<AccountRow> for AccountMeta {
    type Error = anyhow::Error;

    fn try_from(r: AccountRow) -> Result<Self> {
        Ok(Self {
            path: r.path,
            label: r.label,
            account_type: field("account type", &r.acct_type)?,
            closed: r.closed != 0,
        })
    }
}

impl TryFrom<PostingRow> for PostingEntry {
    type Error = anyhow::Error;

    fn try_from(r: PostingRow) -> Result<Self> {
        Ok(Self {
            account_id: r.account_id,
            date: field("date", &r.date)?,
            currency: field("currency", &r.currency)?,
            amount: field("amount", &r.amount)?,
        })
    }
}

impl LedgerData {
    /// The only IO in the balance/net-worth/periodic path; everything
    /// downstream is a pure function of the returned snapshot.
    pub async fn load(pool: &SqlitePool) -> Result<Self> {
        Self::load_from(&mut *pool.acquire().await?).await
    }

    /// [`load`](Self::load) on one connection, so a caller can read its own
    /// uncommitted transaction (the import gate).
    pub async fn load_from(conn: &mut SqliteConnection) -> Result<Self> {
        let account_rows: Vec<AccountRow> =
            sqlx::query_as("SELECT id, path, label, type AS acct_type, closed FROM accounts")
                .fetch_all(&mut *conn)
                .await
                .context("loading accounts")?;
        let accounts = account_rows
            .into_iter()
            .map(|r| Ok((r.id, AccountMeta::try_from(r)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;

        let posting_rows: Vec<PostingRow> = sqlx::query_as(
            "SELECT p.account_id AS account_id, t.date AS date, p.currency AS currency, p.amount \
             AS amount FROM postings p JOIN transactions t ON t.id = p.transaction_id",
        )
        .fetch_all(&mut *conn)
        .await
        .context("loading postings")?;
        let postings =
            posting_rows.into_iter().map(PostingEntry::try_from).collect::<Result<Vec<_>>>()?;
        // The reports index `accounts` by every posting's account; a posting
        // that names none would otherwise vanish from them silently.
        if let Some(orphan) = postings.iter().find(|p| !accounts.contains_key(&p.account_id)) {
            anyhow::bail!("a posting names account id {}, which does not exist", orphan.account_id);
        }

        Ok(Self { accounts, postings })
    }

    /// Every account's label by path, including the group levels with no
    /// postings of their own.
    pub fn labels(&self) -> BTreeMap<&str, &str> {
        self.accounts.values().map(|a| (a.path.as_str(), a.label.as_str())).collect()
    }

    /// The newest posting on each account and currency.
    pub fn last_posting_dates(&self) -> BTreeMap<(i64, Currency), NaiveDate> {
        let mut latest: BTreeMap<(i64, Currency), NaiveDate> = BTreeMap::new();
        for p in &self.postings {
            let entry = latest.entry((p.account_id, p.currency)).or_insert(p.date);
            *entry = (*entry).max(p.date);
        }
        latest
    }

    /// Every currency the ledger holds.
    pub fn currencies(&self) -> BTreeSet<Currency> {
        self.postings.iter().map(|p| p.currency).collect()
    }

    /// Per-account, per-currency balances as of `as_of`: sum of every posting
    /// dated on or before it (openings included — they are postings against
    /// `Equity:Opening-Balances`). Accounts with no activity do not appear.
    pub fn balances_as_of(&self, as_of: NaiveDate) -> Vec<AccountBalance> {
        let mut sums: BTreeMap<(i64, Currency), Decimal> = BTreeMap::new();
        for p in self.postings.iter().filter(|p| p.date <= as_of) {
            *sums.entry((p.account_id, p.currency)).or_default() += p.amount;
        }

        let mut out: Vec<AccountBalance> = sums
            .into_iter()
            .map(|((account_id, currency), amount)| {
                let meta = &self.accounts[&account_id];
                AccountBalance {
                    account_id,
                    path: meta.path.clone(),
                    label: meta.label.clone(),
                    account_type: meta.account_type,
                    closed: meta.closed,
                    currency,
                    amount,
                    as_of,
                }
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path).then(a.currency.cmp(&b.currency)));
        out
    }

    /// Net worth as of a date: the balances [`in_net_worth`] counts, converted
    /// into `base` at the rate in effect on that date.
    pub fn net_worth_as_of(
        &self,
        base: Currency,
        at_cost: &AtCost,
        as_of: NaiveDate,
        prices: &PriceTable,
    ) -> Result<NetWorthPoint> {
        let mut assets = Decimal::ZERO;
        let mut liabilities = Decimal::ZERO;
        for b in self.balances_as_of(as_of).into_iter().filter(|b| in_net_worth(b, at_cost)) {
            // Only the arms that count are converted, so a currency seen solely
            // on an excluded account needs no rate.
            let convert = || convert_as_of(b.amount, b.currency, base, as_of, prices);
            match b.account_type {
                AccountType::Asset => assets += convert()?,
                AccountType::Liability => liabilities += convert()?,
                AccountType::Equity | AccountType::Income | AccountType::Expense => {}
            }
        }
        Ok(NetWorthPoint { date: as_of, assets, liabilities, net: assets + liabilities })
    }

    /// Income and expense flow, grouped by period over `[start, end]`. Income
    /// legs are credit-negative, so they are negated into positive revenue;
    /// expense legs are debit-positive already. Everything is converted
    /// into `base` at the posting's own date.
    pub fn periodic_report(
        &self,
        base: Currency,
        start: NaiveDate,
        end: NaiveDate,
        grain: Grain,
        prices: &PriceTable,
    ) -> Result<PeriodicReport> {
        // Seed every period so quiet ones still report a zero row.
        let mut periods: BTreeMap<NaiveDate, (NaiveDate, Decimal, Decimal)> = BTreeMap::new();
        for (period_start, period_end) in generate_periods(start, end, grain) {
            periods.insert(period_start, (period_end, Decimal::ZERO, Decimal::ZERO));
        }

        for p in self.postings.iter().filter(|p| p.date >= start && p.date <= end) {
            let (_, income, expense) = periods
                .get_mut(&period_start_of(p.date, grain))
                .expect("the periods cover every date in [start, end]");
            // Only flow legs are converted, so a balance-sheet leg in a
            // currency with no rate does not fail the report.
            let convert = || convert_as_of(p.amount, p.currency, base, p.date, prices);
            match self.accounts[&p.account_id].account_type {
                AccountType::Income => *income -= convert()?,
                AccountType::Expense => *expense += convert()?,
                AccountType::Asset | AccountType::Liability | AccountType::Equity => {}
            }
        }

        // Clamp a partial first/last period's reported bounds to the requested
        // range, so its dates match the postings actually summed into it (the
        // filter above already restricts to `[start, end]`).
        let periods = periods
            .into_iter()
            .map(|(period_start, (period_end, income, expense))| Period {
                start: period_start.max(start),
                end: period_end.min(end),
                income,
                expense,
                net: income - expense,
            })
            .collect();
        Ok(PeriodicReport { base, grain, periods })
    }
}

// --- Database-backed entry points ----------------------------------------

/// FX quotes keyed by yfinance-style ticker (e.g. `TWD=X`), each vector sorted
/// by ascending date. The same shape the `portfolio` module already speaks.
pub type PriceTable = std::collections::HashMap<String, Vec<StockPrice>>;

/// Per-account, per-currency balances as of a date.
pub async fn account_balances(pool: &SqlitePool, as_of: NaiveDate) -> Result<Vec<AccountBalance>> {
    Ok(LedgerData::load(pool).await?.balances_as_of(as_of))
}

/// Net worth sampled at the end of each period across `[start, end]`, in
/// `base`. FX rates are read from the `stock_prices` table.
pub async fn net_worth_over_time(
    pool: &SqlitePool,
    base: Currency,
    at_cost: &AtCost,
    start: NaiveDate,
    end: NaiveDate,
    grain: Grain,
) -> Result<NetWorthSeries> {
    let data = LedgerData::load(pool).await?;
    let prices = load_fx_prices(pool, &data.currencies(), base, end).await?;
    let points = generate_periods(start, end, grain)
        .into_iter()
        .map(|(_, period_end)| data.net_worth_as_of(base, at_cost, period_end.min(end), &prices))
        .collect::<Result<_>>()?;
    Ok(NetWorthSeries { base, grain, points })
}

/// Income/expense flow grouped by period across `[start, end]`, in `base`. FX
/// rates are read from the `stock_prices` table.
pub async fn periodic_report(
    pool: &SqlitePool,
    base: Currency,
    start: NaiveDate,
    end: NaiveDate,
    grain: Grain,
) -> Result<PeriodicReport> {
    let data = LedgerData::load(pool).await?;
    let prices = load_fx_prices(pool, &data.currencies(), base, end).await?;
    data.periodic_report(base, start, end, grain, &prices)
}

/// Loads the FX pairs needed to quote every ledger currency in `base` from the
/// shared `stock_prices` table, up to `end`. Both directions of each pair are
/// fetched so an inverse rate can stand in when only one side was recorded.
pub async fn load_fx_prices(
    pool: &SqlitePool,
    currencies: &BTreeSet<Currency>,
    base: Currency,
    end: NaiveDate,
) -> Result<PriceTable> {
    let mut tickers: BTreeSet<String> = BTreeSet::new();
    for &c in currencies.iter().filter(|&&c| c != base) {
        tickers.extend(YFinanceSource::get_exchange_rate_ticker(c, base));
        tickers.extend(YFinanceSource::get_exchange_rate_ticker(base, c));
    }
    match tickers.is_empty() {
        true => Ok(PriceTable::new()),
        false => {
            let owned: Vec<String> = tickers.into_iter().collect();
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            // A date far enough back to pick up every recorded quote.
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid epoch date");
            crate::quotes::StockPriceStore::new(pool.clone())
                .get_stock_prices_in_range(&refs, epoch, end)
                .await
                .context("loading FX rates from stock_prices")
        }
    }
}

// --- Holdings, via the portfolio module ----------------------------------

/// Projects the `portfolio` module's computed holdings into typed report rows.
///
/// The cost-basis, mark-to-market and P&L maths is *not* reimplemented: this
/// drives [`Portfolio::generate_daily_statements`] for the single day `as_of`
/// and translates the resulting [`Statement`]/[`Holding`] values. It converts
/// every currency total at the `as_of` rate (not the newest quote, so a
/// historical report does not drift as new quotes arrive) and restores the
/// caller's `daily_statements`, so it neither drifts nor mutates the portfolio
/// it read. Positions live in the portfolio pipeline, not yet in the SQLite
/// postings (no lot/cost columns — see the gaps note), so this takes a ready
/// `Portfolio` and price table rather than reading the database.
pub fn holdings_report(
    portfolio: &mut Portfolio,
    as_of: NaiveDate,
    prices: &PriceTable,
) -> Result<HoldingsReport> {
    // generate_daily_statements clears the map; save and restore it so this read
    // does not discard statements the caller built over some other range.
    let saved = std::mem::take(&mut portfolio.daily_statements);
    portfolio.generate_daily_statements(as_of, as_of, prices);
    let statement: Statement = portfolio.daily_statements.get(&as_of).cloned().unwrap_or_default();
    portfolio.daily_statements = saved;

    let reporting_currency = portfolio.reporting_currency;

    let mut positions: Vec<HoldingPosition> = statement
        .holdings
        .iter()
        .map(|(symbol, holding)| position_from_holding(symbol, holding, portfolio))
        .collect();
    positions.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    let convert = |amounts: Vec<(Currency, Decimal)>| -> Result<Decimal> {
        amounts
            .into_iter()
            .map(|(c, amount)| convert_as_of(amount, c, reporting_currency, as_of, prices))
            .sum()
    };
    let holdings = |f: fn(&Holding) -> Decimal| {
        statement.holdings.values().map(|h| (h.currency, f(h))).collect::<Vec<_>>()
    };
    let total_cost = convert(holdings(|h| h.total_cost))?;
    let total_market_value = convert(holdings(|h| h.market_value))?;
    let total_unrealized_pnl = convert(holdings(|h| h.unrealized_pnl_value))?;
    let total_realized_pnl = convert(holdings(|h| h.realized_pnl_value))?;
    let total_cash = convert(statement.cash_balances.iter().map(|(c, a)| (*c, *a)).collect())?;

    Ok(HoldingsReport {
        as_of,
        reporting_currency,
        positions,
        total_cost,
        total_market_value,
        total_unrealized_pnl,
        total_realized_pnl,
        total_cash,
        total_value: total_market_value + total_cash,
    })
}

fn position_from_holding(
    symbol: &str,
    holding: &Holding,
    portfolio: &Portfolio,
) -> HoldingPosition {
    let description =
        portfolio.securities.get(symbol).map(|s| s.description.clone()).unwrap_or_default();
    HoldingPosition {
        symbol: symbol.to_string(),
        description,
        currency: holding.currency,
        quantity: holding.quantity,
        total_cost: holding.total_cost,
        average_cost: holding.average_cost,
        market_price: holding.market_price,
        market_value: holding.market_value,
        unrealized_pnl_value: holding.unrealized_pnl_value,
        unrealized_pnl_percentage: holding.unrealized_pnl_percentage,
        realized_pnl_value: holding.realized_pnl_value,
    }
}

// --- FX and calendar helpers ---------------------------------------------

/// `amount` expressed in `to`. Zero converts to zero whatever the rate, so a
/// position that closed out does not need a quote to be reported.
fn convert_as_of(
    amount: Decimal,
    from: Currency,
    to: Currency,
    as_of: NaiveDate,
    prices: &PriceTable,
) -> Result<Decimal> {
    match amount.is_zero() {
        true => Ok(Decimal::ZERO),
        false => Ok(amount * rate_as_of(from, to, as_of, prices)?),
    }
}

/// The rate to multiply a `from` amount by to express it in `to`, as of a date.
/// Tries the direct ticker, then the inverse. A missing pair is an error, never
/// 1:1: that would silently count a foreign amount as the base currency.
fn rate_as_of(
    from: Currency,
    to: Currency,
    as_of: NaiveDate,
    prices: &PriceTable,
) -> Result<Decimal> {
    let inverse =
        || ticker_rate(to, from, as_of, prices).filter(|r| !r.is_zero()).map(|r| Decimal::ONE / r);
    match (from == to, ticker_rate(from, to, as_of, prices).or_else(inverse)) {
        (true, _) => Ok(Decimal::ONE),
        (false, Some(rate)) => Ok(rate),
        (false, None) => anyhow::bail!(
            "no {from}→{to} exchange rate in stock_prices (needed for {as_of}); fetch it with \
             `portfolio rates`"
        ),
    }
}

/// `rate_as_of` as an option: a page that shows several currencies names the
/// unpriced ones instead of failing whole.
pub fn rate(
    from: Currency,
    to: Currency,
    as_of: NaiveDate,
    prices: &PriceTable,
) -> Option<Decimal> {
    rate_as_of(from, to, as_of, prices).ok()
}

/// The recorded rate for one ticker as of a date: the last quote dated on or
/// before `as_of`, or the earliest quote if the date precedes all of them.
fn ticker_rate(
    from: Currency,
    to: Currency,
    as_of: NaiveDate,
    prices: &PriceTable,
) -> Option<Decimal> {
    YFinanceSource::get_exchange_rate_ticker(from, to)
        .and_then(|ticker| prices.get(&ticker))
        .and_then(|quotes| {
            let idx = quotes.partition_point(|p| p.date <= as_of);
            if idx > 0 {
                quotes.get(idx - 1)
            } else {
                quotes.first()
            }
        })
        .and_then(|quote| Decimal::from_f64(quote.close_price))
}

/// True when a balance adds to net worth: an asset or liability not carried
/// at cost. Equity is out, so the `Equity:Conversions` plug never counts.
pub fn in_net_worth(b: &AccountBalance, at_cost: &AtCost) -> bool {
    matches!(b.account_type, AccountType::Asset | AccountType::Liability)
        && !at_cost.covers(&b.path)
}

/// The first day of the period a date falls in.
fn period_start_of(date: NaiveDate, grain: Grain) -> NaiveDate {
    match grain {
        Grain::Day => date,
        Grain::Week => date - Duration::days(date.weekday().num_days_from_monday() as i64),
        Grain::Month => date.with_day(1).expect("day 1 is always valid"),
        Grain::Quarter => {
            let first_month = (date.month0() / 3) * 3 + 1;
            NaiveDate::from_ymd_opt(date.year(), first_month, 1).expect("quarter start is valid")
        }
        Grain::Year => NaiveDate::from_ymd_opt(date.year(), 1, 1).expect("Jan 1 is valid"),
    }
}

/// The last day of the period that starts on `start`.
fn period_end_of(start: NaiveDate, grain: Grain) -> NaiveDate {
    let next = match grain {
        Grain::Day => start + Duration::days(1),
        Grain::Week => start + Duration::days(7),
        Grain::Month => start + Months::new(1),
        Grain::Quarter => start + Months::new(3),
        Grain::Year => start + Months::new(12),
    };
    next - Duration::days(1)
}

/// Every `(start, end)` period that overlaps `[range_start, range_end]`, in
/// order. The first period may begin before `range_start` and the last may end
/// after `range_end`; callers clamp the reporting date where that matters.
fn generate_periods(
    range_start: NaiveDate,
    range_end: NaiveDate,
    grain: Grain,
) -> Vec<(NaiveDate, NaiveDate)> {
    let mut out = Vec::new();
    let mut start = period_start_of(range_start, grain);
    while start <= range_end {
        let end = period_end_of(start, grain);
        out.push((start, end));
        start = end + Duration::days(1);
    }
    out
}
