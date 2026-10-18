//! One account's page, as Fava's account page: where it stands, its balance
//! (or, for income and expense, what moved) month by month, the outside
//! figures that vouch for it, and its journal.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::use_query_map;

use crate::{
    balance_sheet::MoneyText,
    error::LoadFailed,
    journal::JournalList,
    model::{AccountPage, BalanceCheck, JournalQuery},
    plot::{ColumnChart, FigureText, LineChart},
};

#[server]
pub async fn load_account(query: JournalQuery) -> Result<AccountPage, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let today = chrono::Local::now().date_naive();
    crate::detail::load(&pool, &query, today, Currency::TWD)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn AccountRoute() -> impl IntoView {
    let params = use_query_map();
    let page = Resource::new(move || JournalQuery::from(&params.get()), load_account);
    view! {
        <Title text="口座" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match page.await {
                    Ok(page) => view! { <AccountView page /> }.into_any(),
                    Err(e) => view! { <LoadFailed error=e /> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn AccountView(page: AccountPage) -> AnyView {
    let AccountPage { path, label, trail, balance, total, flow, trends, checks, journal } = page;
    // A figure per currency only says more than the total when there is a
    // currency besides the base, or more than one.
    let native = (balance.iter().any(|m| m.currency != total.money.currency) || balance.len() > 1)
        .then(|| {
            let text = balance.iter().map(ToString::to_string).collect::<Vec<_>>().join(" · ");
            view! { <span class="native">{text}</span> }
        });
    let several = trends.len() > 1;
    let charts = trends
        .into_iter()
        .map(|trend| {
            let heading = several.then(|| view! { <h3>{trend.series[0].name.clone()}</h3> });
            let chart = match flow {
                true => view! { <ColumnChart trend /> },
                false => view! { <LineChart trend /> },
            };
            view! { {heading} {chart} }
        })
        .collect_view();
    let checks = (!flow).then(|| view! { <Checks checks /> });
    view! {
        <Title text=label.clone() />
        <div class="account-page">
            <header class="account-head">
                <p class="trail">{trail}</p>
                <h1>{label}</h1>
                <p class="standing">
                    <FigureText figure=total />
                    {native}
                </p>
                <a href=format!("/journal{}", JournalQuery::account(&path))>"去日記帳篩選"</a>
            </header>
            <section>
                <h2>{if flow { "每月金額" } else { "餘額走勢" }}</h2>
                {charts}
            </section>
            {checks}
            <section class="journal">
                <h2>"交易"</h2>
                <JournalList action="/account" journal />
            </section>
        </div>
    }
    .into_any()
}

/// The figures statements, 天天記帳 and counts state for the account, each
/// against what the books say that day.
#[component]
fn Checks(checks: Vec<BalanceCheck>) -> impl IntoView {
    let body = match checks.is_empty() {
        true => view! { <p class="note">"無對帳紀錄"</p> }.into_any(),
        false => {
            let named = checks.iter().any(|c| c.account.is_some());
            let rows = checks
                .into_iter()
                .map(|c| {
                    let account =
                        named.then(|| view! { <td>{c.account.clone().unwrap_or_default()}</td> });
                    let result = match c.ok {
                        true => view! { <span class="check ok">"✓ 相符"</span> }.into_any(),
                        false => view! {
                            <span class="check failed">{format!("✗ 差 {}", c.difference)}</span>
                        }
                        .into_any(),
                    };
                    view! {
                        <tr class:failed=!c.ok>
                            <th scope="row">{c.date}</th>
                            <td>{c.voucher.to_string()}</td>
                            {account}
                            <td class="figure"><MoneyText money=c.expected /></td>
                            <td class="figure"><MoneyText money=c.computed /></td>
                            <td>{result}</td>
                        </tr>
                    }
                })
                .collect_view();
            view! {
                <div class="scroll">
                    <table class="checks">
                        <thead>
                            <tr>
                                <th scope="col">"日期"</th>
                                <th scope="col">"來源"</th>
                                {named.then(|| view! { <th scope="col">"口座"</th> })}
                                <th scope="col">"應有"</th>
                                <th scope="col">"帳上"</th>
                                <th scope="col">"結果"</th>
                            </tr>
                        </thead>
                        <tbody>{rows}</tbody>
                    </table>
                </div>
            }
            .into_any()
        }
    };
    view! {
        <section>
            <h2>"對帳"</h2>
            {body}
        </section>
    }
}
