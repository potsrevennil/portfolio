//! The landing page: 資產 and 負債 as fold-out account trees, per-currency
//! sums, and net worth in the base currency.

use std::collections::BTreeSet;

use leptos::{prelude::*, web_sys::HtmlDetailsElement};
use leptos_meta::Title;

use crate::model::{BalanceSheet, Converted, Money, Node};

/// Where the open groups are remembered across visits, per browser.
const OPEN_KEY: &str = "balance-sheet-open";

#[server]
pub async fn load_balance_sheet() -> Result<BalanceSheet, ServerFnError> {
    use portfolio::currency::Currency;

    let pool = expect_context::<sqlx::SqlitePool>();
    let today = chrono::Local::now().date_naive();
    crate::sheet::load(&pool, today, Currency::TWD)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn BalanceSheetPage() -> impl IntoView {
    let sheet = Resource::new(|| (), |_| load_balance_sheet());
    view! {
        <Title text="資產負債" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match sheet.await {
                    Ok(sheet) => view! { <SheetView sheet /> }.into_any(),
                    Err(e) => view! { <p class="note error">{format!("讀取失敗：{e}")}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn SheetView(sheet: BalanceSheet) -> impl IntoView {
    let open = RwSignal::new(BTreeSet::<String>::new());
    // Client only: restore the open groups once, then save every change.
    Effect::new(move |restored: Option<()>| {
        match restored {
            None => {
                if let Some(saved) = load_open() {
                    open.set(saved);
                }
            }
            Some(()) => save_open(&open.read()),
        }
        open.track();
    });

    let mut groups = BTreeSet::new();
    for s in &sheet.sections {
        collect_groups(&s.nodes, &mut groups);
    }
    let has_groups = !groups.is_empty();

    view! {
        <div class="sheet">
            <div class="tools">
                <span class="as-of">{sheet.as_of}</span>
                <Show when=move || has_groups>
                    <button type="button" on:click={
                        let groups = groups.clone();
                        move |_| open.set(groups.clone())
                    }>"全部展開"</button>
                    <button type="button" on:click=move |_| open.set(BTreeSet::new())>"全部收合"</button>
                </Show>
            </div>
            {sheet
                .sections
                .into_iter()
                .map(|s| {
                    view! {
                        <section>
                            <div class="row section">
                                <span class="name">{s.label}</span>
                                <Figures amounts=s.amounts converted=Some(s.converted) />
                            </div>
                            {s.nodes.into_iter().map(|n| node(n, 0, open)).collect_view()}
                        </section>
                    }
                })
                .collect_view()}
            <div class="row net">
                <span class="name">"淨資產"</span>
                <span class="figures">
                    <MoneyText money=sheet.net_worth.money />
                    <Unpriced currencies=sheet.net_worth.unpriced />
                </span>
            </div>
        </div>
    }
}

/// Paths of the nodes that fold (have children).
fn collect_groups(nodes: &[Node], out: &mut BTreeSet<String>) {
    for n in nodes.iter().filter(|n| !n.children.is_empty()) {
        out.insert(n.path.clone());
        collect_groups(&n.children, out);
    }
}

/// One tree row; a node with children folds.
fn node(n: Node, depth: usize, open: RwSignal<BTreeSet<String>>) -> AnyView {
    let indent = format!("padding-left: calc({depth} * 1.1rem + 0.6rem)");
    let row = || {
        view! {
            <span class="marker" aria-hidden="true"></span>
            <span class="name">{n.label.clone()}</span>
            <Figures amounts=n.amounts.clone() converted=n.converted.clone() />
        }
    };
    if n.children.is_empty() {
        return view! { <div class="row" style=indent>{row()}</div> }.into_any();
    }
    let path = n.path.clone();
    let is_open = {
        let path = path.clone();
        move || open.with(|o| o.contains(&path))
    };
    let head = row();
    let children = n.children.into_iter().map(|c| node(c, depth + 1, open)).collect_view();
    view! {
        <details
            class="group"
            prop:open=is_open
            on:toggle=move |ev| {
                let now = event_target::<HtmlDetailsElement>(&ev).open();
                if open.with_untracked(|o| o.contains(&path)) != now {
                    open.update(|o| {
                        if now { o.insert(path.clone()) } else { o.remove(&path) };
                    });
                }
            }
        >
            <summary class="row" style=indent>{head}</summary>
            {children}
        </details>
    }
    .into_any()
}

#[component]
fn Figures(amounts: Vec<Money>, converted: Option<Converted>) -> impl IntoView {
    let amounts = match amounts.is_empty() {
        true => vec![Money { text: "0".into(), negative: false }],
        false => amounts,
    };
    view! {
        <span class="figures">
            <span class="amounts">
                {amounts.into_iter().map(|money| view! { <MoneyText money /> }).collect_view()}
            </span>
            {converted
                .map(|c| {
                    view! {
                        <span class="converted">
                            "≈ " <MoneyText money=c.money /> <Unpriced currencies=c.unpriced />
                        </span>
                    }
                })}
        </span>
    }
}

#[component]
fn MoneyText(money: Money) -> impl IntoView {
    view! { <span class="money" class:negative=money.negative>{money.text}</span> }
}

#[component]
fn Unpriced(currencies: Vec<String>) -> impl IntoView {
    (!currencies.is_empty())
        .then(|| view! { <span class="unpriced">{format!("（未換算：{}）", currencies.join("、"))}</span> })
}

fn storage() -> Option<leptos::web_sys::Storage> { window().local_storage().ok().flatten() }

fn load_open() -> Option<BTreeSet<String>> {
    let json = storage()?.get_item(OPEN_KEY).ok()??;
    serde_json::from_str(&json).ok()
}

fn save_open(open: &BTreeSet<String>) {
    if let (Some(storage), Ok(json)) = (storage(), serde_json::to_string(open)) {
        // Private windows may refuse; the fold state is a convenience.
        let _ = storage.set_item(OPEN_KEY, &json);
    }
}
