//! 資產 and 負債 as fold-out account trees, per-currency sums, and net worth.

use std::collections::BTreeSet;

use leptos::{prelude::*, web_sys::HtmlDetailsElement};
use leptos_meta::Title;

use crate::model::{BalanceSheet, Converted, Money, Node, Unpriced};

/// Where the open groups are remembered across visits, per browser.
const OPEN_KEY: &str = "balance-sheet-open";

#[server]
pub async fn load_balance_sheet() -> Result<BalanceSheet, ServerFnError> {
    use portfolio::currency::Currency;

    let pool = expect_context::<sqlx::SqlitePool>();
    let at_cost = expect_context::<portfolio::ledger::valuation::AtCost>();
    let today = chrono::Local::now().date_naive();
    crate::sheet::load(&pool, today, Currency::TWD, &at_cost)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn BalanceSheetPage() -> impl IntoView {
    let sheet = Resource::new(|| (), |_| load_balance_sheet());
    view! {
        <Title text="資產負債表" />
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
    // Sections start open; the groups under them start folded.
    let sections: BTreeSet<String> = sheet.sections.iter().map(|s| s.path.clone()).collect();
    let open = RwSignal::new(sections.clone());
    // Client only: restore the open groups once, then save every change. The
    // `open` attribute is rendered on the server, and hydration adopts the
    // markup as it stands, so a restore has to land after it — the next frame.
    Effect::new(move |restored: Option<()>| {
        match restored {
            None => request_animation_frame(move || {
                if let Some(saved) = load_open() {
                    open.set(saved);
                }
            }),
            Some(()) => save_open(&open.read()),
        }
        open.track();
    });

    let mut groups = sections.clone();
    for s in &sheet.sections {
        collect_groups(&s.nodes, &mut groups);
    }
    let has_groups = groups.len() > sections.len();

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
            <div class="columns">
                {sheet
                    .sections
                    .into_iter()
                .map(|s| {
                    let path = s.path.clone();
                    let is_open = {
                        let path = path.clone();
                        move || open.with(|o| o.contains(&path))
                    };
                    view! {
                        <details class="group" open=is_open on:toggle=on_toggle(path, open)>
                            <summary class="row section">
                                <span class="marker" aria-hidden="true"></span>
                                <span class="name">{s.label}</span>
                                <Figures
                                    amounts=s.amounts
                                    converted=s.converted
                                    excluded=s.excluded
                                />
                            </summary>
                            {s.nodes.into_iter().map(|n| node(n, 0, open)).collect_view()}
                        </details>
                    }
                })
                    .collect_view()}
            </div>
            <div class="row net">
                <span class="name">"淨資產"</span>
                <span class="figures">
                    <span class="amounts">
                        <MoneyText money=sheet.net_worth.money />
                    </span>
                    <UnpricedNote unpriced=sheet.net_worth.unpriced />
                    <Excluded excluded=sheet.excluded />
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
    // A group's own row carries the size of its level, so the tree reads as a
    // hierarchy rather than a list.
    let level = format!("row level-{}", depth.min(2));
    let row = || {
        view! {
            <span class="marker" aria-hidden="true"></span>
            <span class="name">{n.label.clone()}</span>
            <Figures
                amounts=n.amounts.clone()
                converted=n.converted.clone()
                at_cost=n.at_cost
            />
        }
    };
    if n.children.is_empty() {
        return view! { <div class=level.clone() style=indent>{row()}</div> }.into_any();
    }
    let path = n.path.clone();
    let is_open = {
        let path = path.clone();
        move || open.with(|o| o.contains(&path))
    };
    let head = row();
    let children = n.children.into_iter().map(|c| node(c, depth + 1, open)).collect_view();
    view! {
        <details class="group" open=is_open on:toggle=on_toggle(path, open)>
            <summary class=level.clone() style=indent>{head}</summary>
            {children}
        </details>
    }
    .into_any()
}

/// Keeps the fold state in step with a `<details>` the reader just toggled.
fn on_toggle(path: String, open: RwSignal<BTreeSet<String>>) -> impl Fn(leptos::web_sys::Event) {
    move |ev: leptos::web_sys::Event| {
        let now = event_target::<HtmlDetailsElement>(&ev).open();
        if open.with_untracked(|o| o.contains(&path)) != now {
            open.update(|o| {
                if now {
                    o.insert(path.clone())
                } else {
                    o.remove(&path)
                };
            });
        }
    }
}

#[component]
fn Figures(
    amounts: Vec<Money>,
    converted: Option<Converted>,
    /// A cost rather than a valuation, so it is labelled as one.
    #[prop(default = false)]
    at_cost: bool,
    /// The at-cost holdings this total leaves out.
    #[prop(default = None)]
    excluded: Option<Converted>,
) -> impl IntoView {
    // A group whose currencies all net to zero still has a row to fill, and no
    // currency to name it in — as Fava prints it.
    let zero = amounts.is_empty().then(|| view! { <span class="money">"0"</span> });
    view! {
        <span class="figures">
            <span class="amounts">
                {zero}
                {amounts.into_iter().map(|money| view! { <MoneyText money /> }).collect_view()}
            </span>
            {converted
                .map(|c| {
                    view! {
                        <span class="converted">
                            "≈ " <MoneyText money=c.money /> <UnpricedNote unpriced=c.unpriced />
                        </span>
                    }
                })}
            {at_cost.then(|| view! { <span class="at-cost">"成本，未計入總額"</span> })}
            <Excluded excluded=excluded />
        </span>
    }
}

/// What a total leaves out because it is held at cost.
#[component]
pub fn Excluded(excluded: Option<Converted>) -> impl IntoView {
    excluded.map(|c| {
        view! {
            <span class="at-cost">
                {format!("另有成本 {}", c.money)} <UnpricedNote unpriced=c.unpriced />
            </span>
        }
    })
}

#[component]
pub fn MoneyText(money: Money) -> impl IntoView {
    view! { <span class="money" class:negative=money.is_negative()>{money.to_string()}</span> }
}

#[component]
pub fn UnpricedNote(unpriced: Unpriced) -> impl IntoView {
    view! { <span class="unpriced">{unpriced.to_string()}</span> }
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
