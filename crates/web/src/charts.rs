//! 圖表: net worth over time, income against expense per period, and where
//! the money came from and went — Fava's balance-sheet and income-statement
//! charts.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::{components::Form, hooks::use_query_map};

use crate::{
    error::LoadFailed,
    model::{ChartQuery, Charts, Step},
    plot::{ColumnChart, LineChart, ShareBars},
};

#[server]
pub async fn load_charts(query: ChartQuery) -> Result<Charts, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let at_cost = expect_context::<ledger::valuation::AtCost>();
    let today = chrono::Local::now().date_naive();
    crate::trends::load(&pool, &query, today, Currency::TWD, &at_cost)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn ChartsPage() -> impl IntoView {
    let params = use_query_map();
    let charts = Resource::new(move || ChartQuery::from(&params.get()), load_charts);
    view! {
        <Title text="圖表" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match charts.await {
                    Ok(charts) => view! { <ChartsView charts /> }.into_any(),
                    Err(e) => view! { <LoadFailed error=e /> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn ChartsView(charts: Charts) -> impl IntoView {
    let Charts { query, worth, flows, income, expense } = charts;
    let step = query.step.clone().unwrap_or_default();
    let steps = Step::ALL
        .into_iter()
        .map(|s| {
            let value = s.to_string();
            view! {
                <option value=value.clone() selected=value == step>
                    {s.name()}
                </option>
            }
        })
        .collect_view();
    view! {
        <div class="charts">
            <Form method="GET" action="/charts">
                <div class="filter">
                    <label>
                        "從" <input type="date" name="from" value=query.from.unwrap_or_default() />
                    </label>
                    <label>
                        "到" <input type="date" name="to" value=query.to.unwrap_or_default() />
                    </label>
                    <label>"間隔" <select name="step">{steps}</select></label>
                    <button type="submit">"篩選"</button>
                    <a href="/charts">"清除"</a>
                </div>
            </Form>
            <section>
                <h2>"淨資產"</h2>
                <LineChart trend=worth />
            </section>
            <section>
                <h2>"收支"</h2>
                <ColumnChart trend=flows />
            </section>
            <div class="split">
                <section>
                    <h2>"支出分類"</h2>
                    <ShareBars share=expense hue=2 />
                </section>
                <section>
                    <h2>"收入分類"</h2>
                    <ShareBars share=income hue=1 />
                </section>
            </div>
        </div>
    }
}
