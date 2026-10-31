//! 記一筆: a cash spend or transfer entered by hand, booked reviewed.

use leptos::prelude::*;
use leptos_meta::Title;

use crate::{
    account_box::{AccountBox, Accounts},
    model::{AccountKind, ChartAccount},
    review::ActionError,
};

/// The accounts the form offers, and today's date to start from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManualForm {
    pub today: String,
    pub accounts: Vec<ChartAccount>,
    /// What a new account may open under.
    pub parents: Vec<ChartAccount>,
}

#[server]
pub async fn load_manual() -> Result<ManualForm, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let unlisted = expect_context::<crate::entries::Unlisted>();
    let (accounts, parents) =
        crate::queue::manual_accounts(&pool, &unlisted).await.map_err(crate::review::failed)?;
    Ok(ManualForm { today: chrono::Local::now().date_naive().to_string(), accounts, parents })
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
    let statements = expect_context::<db::review::StatementAccounts>();
    db::review::enter(&pool, &statements, &manual).await.map_err(crate::review::failed)
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

/// What the entry is: it decides which accounts the form asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Paid from an account, into a spending category.
    Spend,
    /// Received into an account, from an income category.
    Income,
    /// Moved from one account to another.
    Transfer,
}

#[component]
pub fn ManualEntry(form: ManualForm) -> impl IntoView {
    let enter = ServerAction::<EnterManual>::new();
    let kind = RwSignal::new(Kind::Spend);
    let of = |kinds: &[AccountKind]| {
        let kept = |list: &[ChartAccount]| -> Vec<ChartAccount> {
            list.iter().filter(|a| a.kind.is_some_and(|k| kinds.contains(&k))).cloned().collect()
        };
        Accounts::new(kept(&form.accounts), kept(&form.parents))
    };
    let held = of(&[AccountKind::Asset, AccountKind::Liability]);
    let spending = of(&[AccountKind::Expense]);
    let income = of(&[AccountKind::Income]);
    let entered = move || {
        enter.value().get().and_then(Result::ok).map(|id| {
            view! {
                <p class="note done">
                    "已記下。" <a href=format!("/review/{id}")>"看這筆"</a>
                </p>
            }
        })
    };
    let choice = move |value: Kind, text: &'static str| {
        view! {
            <label>
                <input
                    type="radio"
                    name="kind"
                    checked=move || kind.get() == value
                    on:change=move |_| kind.set(value)
                />
                {text}
            </label>
        }
    };
    // Only the accounts this kind of entry needs; the others are not sent.
    let accounts = move || {
        let (from, other, other_name, other_list) = match kind.get() {
            Kind::Spend => ("付款帳戶", "類別", "category", spending),
            Kind::Income => ("收款帳戶", "類別", "category", income),
            Kind::Transfer => ("從", "轉到", "counter", held),
        };
        view! {
            <label class="field">
                {from} <AccountBox name="account".to_string() accounts=held placeholder="請選" />
            </label>
            <label class="field">
                {other} <AccountBox name=other_name.to_string() accounts=other_list placeholder="請選" />
            </label>
        }
    };
    view! {
        <div class="journal manual">
            <ActionForm action=enter>
                <div class="kinds">
                    {choice(Kind::Spend, "支出")} {choice(Kind::Income, "收入")}
                    {choice(Kind::Transfer, "轉帳")}
                </div>
                <div class="form">
                    <label class="field">"日期" <input type="date" name="date" value=form.today required /></label>
                    <label class="field">
                        "金額" <input type="text" inputmode="decimal" name="amount" required />
                    </label>
                    <label class="field">"幣別" <input type="text" class="currency" name="currency" value="TWD" /></label>
                    {accounts}
                    <label class="field">"備註" <input type="text" name="note" /></label>
                </div>
                <button type="submit">"記下"</button>
            </ActionForm>
            {entered}
            <ActionError result=enter.value() />
        </div>
    }
}
