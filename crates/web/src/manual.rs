//! 記一筆: a cash spend or transfer entered by hand, booked reviewed.

use leptos::prelude::*;
use leptos_meta::Title;

use crate::{
    model::{AccountChoice, AccountKind},
    review::{AccountSelect, ActionError},
};

/// The accounts the form offers, and today's date to start from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManualForm {
    pub today: String,
    pub accounts: Vec<AccountChoice>,
}

#[server]
pub async fn load_manual() -> Result<ManualForm, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let accounts = crate::queue::manual_accounts(&pool).await.map_err(crate::review::failed)?;
    Ok(ManualForm { today: chrono::Local::now().date_naive().to_string(), accounts })
}

#[server]
pub async fn enter_manual(
    date: String,
    amount: String,
    currency: String,
    account: String,
    category: Option<String>,
    counter: Option<String>,
    note: Option<String>,
) -> Result<i64, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let input = crate::model::ManualInput {
        date,
        amount,
        currency,
        account,
        category: category.unwrap_or_default(),
        counter: counter.unwrap_or_default(),
        note: note.unwrap_or_default(),
    };
    let manual = db::review::Manual::try_from(&input).map_err(crate::review::failed)?;
    db::review::enter(&pool, &manual).await.map_err(crate::review::failed)
}

#[component]
pub fn ManualPage() -> impl IntoView {
    let form = Resource::new(|| (), |_| load_manual());
    view! {
        <Title text="記一筆" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match form.await {
                    Ok(form) => view! { <ManualEntry form /> }.into_any(),
                    Err(e) => view! { <crate::error::LoadFailed error=e /> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn ManualEntry(form: ManualForm) -> impl IntoView {
    let enter = ServerAction::<EnterManual>::new();
    let of = |kinds: &[AccountKind]| -> Vec<AccountChoice> {
        form.accounts.iter().filter(|a| kinds.contains(&a.kind)).cloned().collect()
    };
    let held = of(&[AccountKind::Asset, AccountKind::Liability]);
    let categories = of(&[AccountKind::Expense, AccountKind::Income]);
    let entered = move || {
        enter.value().get().and_then(Result::ok).map(|id| {
            view! {
                <p class="note done">
                    "已記下。" <a href=format!("/review/{id}")>"看這筆"</a>
                </p>
            }
        })
    };
    view! {
        <div class="journal manual">
            <ActionForm action=enter>
                <div class="form">
                    <label class="field">"日期" <input type="date" name="date" value=form.today required /></label>
                    <label class="field">
                        "金額" <input type="text" inputmode="decimal" name="amount" required />
                    </label>
                    <label class="field">"幣別" <input type="text" class="currency" name="currency" value="TWD" /></label>
                    <label class="field">
                        "帳戶" <AccountSelect name="account".to_string() accounts=held.clone() blank="請選" />
                    </label>
                    <label class="field">
                        "類別" <AccountSelect name="category".to_string() accounts=categories />
                    </label>
                    <label class="field">
                        "對方帳戶" <AccountSelect name="counter".to_string() accounts=held />
                    </label>
                    <label class="field">"備註" <input type="text" name="note" /></label>
                </div>
                <p class="note">"類別和對方帳戶選一個：支出從帳戶扣，收入進帳戶，轉帳從帳戶轉到對方帳戶。金額填負的就反過來。"</p>
                <button type="submit">"記下"</button>
            </ActionForm>
            {entered}
            <ActionError result=enter.value() />
        </div>
    }
}
