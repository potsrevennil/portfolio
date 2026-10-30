//! What the page asks of the server, and what its forms change.

use ledger_types::cadence::CadenceKind;
use leptos::prelude::*;

use super::model::Coverage;

#[server]
pub async fn load_coverage() -> Result<Coverage, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let splits = expect_context::<ledger::accounts::SplitAccounts>();
    let today = chrono::Local::now().date_naive();
    super::build::load(&pool, today, &splits, Currency::TWD).await.map_err(crate::review::failed)
}

/// How many accounts await a statement or a count: the nav's badge.
#[server]
pub async fn coverage_due() -> Result<u64, ServerFnError> {
    // The nav renders on every page, including the one pass that gathers
    // the route list, where no database is in context yet.
    let pool = use_context::<db::SqlitePool>().ok_or_else(|| ServerFnError::new("沒有資料庫"))?;
    let today = chrono::Local::now().date_naive();
    super::build::due(&pool, today).await.map_err(crate::review::failed)
}

#[server]
pub async fn save_schedule(
    account: String,
    bank: String,
    kind: CadenceKind,
    month: Option<String>,
    day: Option<String>,
    every: Option<String>,
    grace: String,
    since: Option<String>,
) -> Result<(), ServerFnError> {
    use ledger_types::cadence::{Cadence, CadenceSpec, GRACE};

    use crate::review::failed;

    let filled = |field: Option<String>| field.filter(|f| !f.trim().is_empty());
    let number = |field: Option<String>| -> Result<Option<u32>, ServerFnError> {
        filled(field)
            .map(|n| n.trim().parse().map_err(|_| ServerFnError::new(format!("數字毋著：{n}"))))
            .transpose()
    };
    let spec =
        CadenceSpec { kind, month: number(month)?, day: number(day)?, every: number(every)? };
    let schedule = db::coverage::Schedule {
        account,
        bank: Some(bank),
        cadence: Cadence::try_from(spec).map_err(|e| failed(e.into()))?,
        grace: number(Some(grace))?.unwrap_or(GRACE),
        since: filled(since).as_deref().map(crate::typed::date).transpose().map_err(failed)?,
    };
    let pool = expect_context::<db::SqlitePool>();
    let mut conn = pool.acquire().await?;
    db::coverage::set_schedule(&mut conn, &schedule).await.map_err(failed)
}

/// Records that `account` held `amount` of `currency` at the end of `date`,
/// as the reader confirmed by hand. Where the ledger disagrees it is refused,
/// unless `correct` asks for the difference to be booked first.
#[server]
pub async fn confirm_balance(
    account: String,
    date: String,
    currency: String,
    amount: String,
    correct: Option<String>,
) -> Result<(), ServerFnError> {
    use crate::review::failed;

    let date = crate::typed::date(&date).map_err(failed)?;
    let currency = crate::typed::currency(&currency).map_err(failed)?;
    let amount = crate::typed::amount(&amount).map_err(failed)?;
    let pool = expect_context::<db::SqlitePool>();
    if correct.is_some() {
        let statements = expect_context::<db::review::StatementAccounts>();
        let to = expect_context::<db::coverage::Corrections>();
        db::coverage::book_difference(&pool, &statements, &to, &account, date, currency, amount)
            .await
            .map_err(failed)?;
    }
    let mut tx = pool.begin().await?;
    db::coverage::confirm(&mut tx, &account, date, currency, amount).await.map_err(failed)?;
    tx.commit().await?;
    Ok(())
}

/// The actions the page's forms run; the page and the nav's badge reload
/// whenever one completes.
#[derive(Clone, Copy)]
pub struct CoverageActions {
    pub save: ServerAction<SaveSchedule>,
    pub confirm: ServerAction<ConfirmBalance>,
}

impl CoverageActions {
    pub fn new() -> Self {
        CoverageActions { save: ServerAction::new(), confirm: ServerAction::new() }
    }

    pub(super) fn version(&self) -> usize {
        self.save.version().get() + self.confirm.version().get()
    }
}

impl Default for CoverageActions {
    fn default() -> Self { Self::new() }
}
