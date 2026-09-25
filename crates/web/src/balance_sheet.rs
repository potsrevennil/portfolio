//! Net worth, 資產 and 負債 as fold-out account trees, and the holdings
//! carried at cost, which no total includes.

use std::collections::BTreeSet;

use leptos::{prelude::*, web_sys::HtmlDetailsElement};
use leptos_meta::Title;

use crate::model::{BalanceSheet, Converted, JournalQuery, Money, Node, Section, Unpriced};

/// Where the open groups are remembered across visits, per browser.
const OPEN_KEY: &str = "balance-sheet-open";

#[server]
pub async fn load_balance_sheet() -> Result<BalanceSheet, ServerFnError> {
    use ledger_types::currency::Currency;

    let pool = expect_context::<db::SqlitePool>();
    let at_cost = expect_context::<ledger::valuation::AtCost>();
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
                    Err(e) => view! { <p class="note error">{crate::error::load_failed(&e)}</p> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn SheetView(sheet: BalanceSheet) -> impl IntoView {
    // Sections start open; the groups under them start folded.
    let sections: BTreeSet<String> =
        sheet.sections.iter().chain(&sheet.at_cost).map(|s| s.path.clone()).collect();
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
            <div class="row net">
                <span class="name">"淨資產"</span>
                <span class="figures">
                    <MoneyText money=sheet.net_worth.money />
                    <UnpricedNote unpriced=sheet.net_worth.unpriced />
                </span>
            </div>
            <div class="columns">
                {sheet.sections.into_iter().map(|s| section(s, open)).collect_view()}
            </div>
            {sheet.at_cost.map(|s| view! { <div class="cost-section">{section(s, open)}</div> })}
        </div>
    }
}

/// A section heading over its rows; it folds.
fn section(s: Section, open: RwSignal<BTreeSet<String>>) -> impl IntoView {
    let path = s.path.clone();
    let is_open = is_open(path.clone(), open);
    view! {
        <details class="group" open=is_open on:toggle=on_toggle(path, open)>
            <summary class="row section">
                <span class="marker" aria-hidden="true"></span>
                <span class="name">{s.label}</span>
                <Figures total=s.total />
            </summary>
            {s.nodes.into_iter().map(|n| node(n, 0, open)).collect_view()}
        </details>
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
            <a class="name" href=format!("/journal{}", JournalQuery::account(&n.path))>
                {n.label.clone()}
            </a>
            <Figures total=n.total.clone() native=n.native.clone() />
        }
    };
    if n.children.is_empty() {
        return view! { <div class=level.clone() style=indent>{row()}</div> }.into_any();
    }
    let path = n.path.clone();
    let is_open = is_open(path.clone(), open);
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

/// Whether the group at `path` is open, tracked for the `open` attribute.
fn is_open(path: String, open: RwSignal<BTreeSet<String>>) -> impl Fn() -> bool {
    move || open.with(|o| o.contains(&path))
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

/// One figure in the base currency, whatever the row holds.
#[component]
fn Figures(
    total: Converted,
    /// The account's own foreign balance, the figure its statement shows.
    #[prop(default = Vec::new())]
    native: Vec<Money>,
) -> impl IntoView {
    let native = (!native.is_empty()).then(|| {
        let text = native.iter().map(ToString::to_string).collect::<Vec<_>>().join(" · ");
        view! { <span class="native">{text}</span> }
    });
    view! {
        <span class="figures">
            <MoneyText money=total.money />
            <UnpricedNote unpriced=total.unpriced />
            {native}
        </span>
    }
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
