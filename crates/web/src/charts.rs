//! 總覽, the landing page: where things stand, net worth over time,
//! day-to-day income against expense per month with the net result of
//! investments apart, and one period's flows by category — Fava's
//! balance-sheet and income-statement charts.

use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::use_query_map;

use crate::{
    balance_sheet::TotalText,
    error::LoadFailed,
    model::{Breakdown, ChartQuery, Charts, Headline, PeriodChoice, Tile},
    plot::{ColumnChart, LineChart, PieChart},
};

/// JSON, since a query with nothing set would encode as no field at all.
#[server(input = leptos::server_fn::codec::Json)]
pub async fn load_charts(query: ChartQuery) -> Result<Charts, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let market = expect_context::<db::market::Market>();
    let at_cost = expect_context::<ledger::valuation::AtCost>();
    let investments = expect_context::<ledger::investments::Investments>();
    let today = chrono::Local::now().date_naive();
    let rules = crate::trends::Rules { at_cost: &at_cost, investments: &investments };
    crate::trends::load(&pool, &market, &query, today, Currency::TWD, rules)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn ChartsPage() -> impl IntoView {
    let params = use_query_map();
    let charts = Resource::new(move || ChartQuery::from(&params.get()), load_charts);
    view! {
        <Title text="總覽" />
        // Picking another period keeps the page in place while it loads.
        <Transition fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match charts.await {
                    Ok(charts) => view! { <ChartsView charts /> }.into_any(),
                    Err(e) => view! { <LoadFailed error=e /> }.into_any(),
                }
            })}
        </Transition>
    }
}

#[component]
pub fn ChartsView(charts: Charts) -> impl IntoView {
    let Charts { headline, worth, flows, starts, investments, breakdown, at_cost, .. } = charts;
    // Picking a month of 收支 breaks it down below.
    let links: Vec<String> =
        starts.iter().map(|start| format!("/{}", ChartQuery::period(start))).collect();
    let marked = breakdown.marked.clone();
    let investments = investments.map(|trend| {
        view! {
            <section>
                <h2>"投資損益"</h2>
                <ColumnChart trend />
            </section>
        }
    });
    let at_cost = at_cost.map(|total| {
        view! {
            <p class="aside-note">"以成本衡量之投資 " <TotalText total /> "，不計入淨資產"</p>
        }
    });
    view! {
        <div class="charts">
            <HeadlineView headline />
            <section>
                <h2>"淨資產"</h2>
                <LineChart trend=worth />
                {at_cost}
            </section>
            <section>
                <h2>"收支"</h2>
                <ColumnChart trend=flows links marked />
            </section>
            {investments}
            <BreakdownView breakdown />
        </div>
    }
}

/// The page's title, and where things stand at a glance.
#[component]
fn HeadlineView(headline: Headline) -> impl IntoView {
    let Headline { as_of, tiles } = headline;
    let tiles = tiles
        .into_iter()
        .map(|Tile { label, figure, beside }| {
            let beside = beside.map(|(name, figure)| {
                view! {
                    <span class="beside">
                        {name} " " <TotalText total=figure />
                    </span>
                }
            });
            view! {
                <div class="stat">
                    <span class="name">{label}</span>
                    <span class="value"><TotalText total=figure /></span>
                    {beside}
                </div>
            }
        })
        .collect_view();
    view! {
        <header class="headline">
            <h1>"總覽"</h1>
            <span class="as-of">{as_of}</span>
        </header>
        <div class="stats">{tiles}</div>
    }
}

/// One period's income and expense by category, and the years and months
/// to pick another from.
#[component]
fn BreakdownView(breakdown: Breakdown) -> impl IntoView {
    let Breakdown { name, income, expense, years, periods, .. } = breakdown;
    let chips = |choices: Vec<PeriodChoice>| {
        choices
            .into_iter()
            .map(|c| {
                let href = format!("/{}", ChartQuery::period(&c.key));
                view! {
                    <a
                        class="chip"
                        class:selected=c.selected
                        aria-current=c.selected.then_some("true")
                        href=href
                        data-noscroll="true"
                    >
                        {c.label}
                    </a>
                }
            })
            .collect_view()
    };
    let periods =
        (!periods.is_empty()).then(|| view! { <nav class="chips">{chips(periods)}</nav> });
    view! {
        <section id="breakdown">
            <h2>"收支分類"</h2>
            <p class="subtitle">{name}</p>
            <nav class="chips">{chips(years)}</nav>
            {periods}
            <p class="hint">"也可以點收支的長條，看那個月。"</p>
            <div class="split">
                <PieChart pie=expense />
                <PieChart pie=income />
            </div>
        </section>
    }
}
