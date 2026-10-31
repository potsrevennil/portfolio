//! The account box: walk the tree or type to search the accounts and pick one,
//! or name one that isn't there and say what it goes under.

use std::{collections::BTreeSet, fmt};

use leptos::{
    ev,
    html::{Input, Span, Ul},
    prelude::*,
    wasm_bindgen::JsCast,
    web_sys::Node,
};

use crate::model::{offered_for, ChartAccount};

/// What parts a typed trail's steps: the box's own mark, and the one a keyboard
/// has.
const STEP: [char; 2] = ['›', '>'];

/// Opens an account under `parent`; the page offers it at once.
#[server]
pub async fn open_account(parent: String, name: String) -> Result<ChartAccount, ServerFnError> {
    if parent.trim().is_empty() {
        return Err(ServerFnError::new("請揀欲開佇佗位"));
    }
    let pool = expect_context::<db::SqlitePool>();
    let unlisted = expect_context::<crate::entries::Unlisted>();
    let path =
        db::review::open_account(&pool, &parent, &name).await.map_err(crate::review::failed)?;
    crate::queue::opened(&pool, &unlisted, &path).await.map_err(crate::review::failed)
}

/// What a page's boxes offer, cheap to hand to each. An account opened from
/// one box is offered by all of them.
#[derive(Clone, Copy)]
pub struct Accounts {
    offered: StoredValue<Vec<ChartAccount>>,
    parents: StoredValue<Vec<ChartAccount>>,
    opened: RwSignal<Vec<ChartAccount>>,
}

impl Accounts {
    pub fn new(offered: Vec<ChartAccount>, parents: Vec<ChartAccount>) -> Self {
        Self {
            offered: StoredValue::new(offered),
            parents: StoredValue::new(parents),
            opened: RwSignal::new(Vec::new()),
        }
    }

    /// Every account this box can hold, as of now.
    fn all(self) -> Vec<ChartAccount> {
        let mut all = self.offered.get_value();
        all.extend(self.opened.get_untracked());
        all
    }

    fn opening_under(self) -> Vec<ChartAccount> {
        let mut all = self.parents.get_value();
        all.extend(self.opened.get_untracked());
        all
    }

    /// The places open on arrival: the roots, and the way down to `path`.
    fn opened_for(self, path: &str) -> BTreeSet<String> {
        let roots = self.parents.get_value().into_iter().filter(|a| a.depth == 0).map(|a| a.path);
        roots.chain(path.match_indices(':').map(|(i, _)| path[..i].to_string())).collect()
    }

    fn name_of(self, path: &str) -> String {
        let found = self.all().into_iter().find(|a| a.path == path);
        found.map(|a| a.name).unwrap_or_default()
    }

    /// The tree to walk: what can be picked, among the accounts above them that
    /// are only there to fold.
    fn places(self, picking: Vec<ChartAccount>) -> Vec<Place> {
        let pickable: BTreeSet<String> = picking.iter().map(|a| a.path.clone()).collect();
        let mut seen = BTreeSet::new();
        let mut places: Vec<Place> = picking
            .into_iter()
            .chain(self.parents.get_value())
            .filter(|a| seen.insert(a.path.clone()))
            .map(|account| Place { pickable: pickable.contains(&account.path), account })
            .collect();
        // By step, not by text: `A-B` would otherwise sort inside `A`.
        places.sort_by(|a, b| a.account.path.split(':').cmp(b.account.path.split(':')));
        places
    }
}

/// An account in the tree.
#[derive(Clone)]
struct Place {
    account: ChartAccount,
    pickable: bool,
}

/// A row of the box's list.
#[derive(Clone)]
enum Row {
    /// A search hit, named by its whole trail.
    Found(ChartAccount),
    /// A place in the tree, under its own label; `fold` is whether it is open,
    /// where anything files under it.
    Node { place: Place, fold: Option<bool> },
    /// What was typed names no account: offer to open it.
    New(NewAccount),
}

/// The account some typing would open.
#[derive(Clone)]
struct NewAccount {
    name: String,
    /// Where the typing said it goes; otherwise the box asks.
    under: Option<ChartAccount>,
}

impl NewAccount {
    /// The last step is the name, and the steps before it say where when they
    /// fit one place. Text run straight on from the trail the box `held` goes
    /// under that account.
    fn read(places: &[ChartAccount], held: &str, typed: &str) -> Self {
        let trail = |text: &str| text.split(STEP).map(str::trim).collect::<Vec<_>>().join(" › ");
        let whole = trail(typed);
        let run_on = places.iter().filter(|a| a.path == held).find_map(|a| {
            let name = whole.strip_prefix(a.name.as_str())?;
            (!name.contains(STEP)).then_some((name, a))
        });
        match (run_on, typed.rsplit_once(STEP)) {
            (Some((name, held)), _) => {
                Self { name: name.trim().to_string(), under: Some(held.clone()) }
            }
            (None, Some((place, name))) => {
                let place = trail(place);
                let inside = format!(" › {place}");
                let mut fits =
                    places.iter().filter(|a| a.name == place || a.name.ends_with(&inside));
                let under = fits.next().filter(|_| fits.next().is_none()).cloned();
                Self { name: name.trim().to_string(), under }
            }
            (None, None) => Self { name: typed.trim().to_string(), under: None },
        }
    }

    /// Whether `account` already is what this would open.
    fn taken_by(&self, account: &ChartAccount) -> bool {
        match &self.under {
            Some(under) => account.name == format!("{} › {}", under.name, self.name),
            None => account.label == self.name,
        }
    }
}

impl fmt::Display for NewAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.under {
            Some(under) => write!(f, "＋ 開新口座「{}」佇 {}", self.name, under.name),
            None => write!(f, "＋ 開新口座「{}」", self.name),
        }
    }
}

/// The accounts for what was typed: every word somewhere in the trail, so a
/// name can be narrowed by any level above it.
fn search(accounts: Vec<ChartAccount>, typed: &str, new: Option<NewAccount>) -> Vec<Row> {
    let open = new
        .filter(|new| !new.name.is_empty() && !accounts.iter().any(|a| new.taken_by(a)))
        .map(Row::New);
    let words: Vec<&str> = typed
        .split(|c: char| c.is_whitespace() || STEP.contains(&c))
        .filter(|word| !word.is_empty())
        .collect();
    let found = accounts
        .into_iter()
        .filter(|a| words.iter().all(|word| offered_for(&a.name, word)))
        .map(Row::Found);
    found.chain(open).collect()
}

/// What shows of the tree: everything but what is under a closed place.
fn tree(places: Vec<Place>, opened: &BTreeSet<String>) -> Vec<Row> {
    let under = |path: &str, parent: &str| {
        path.strip_prefix(parent).is_some_and(|rest| rest.starts_with(':'))
    };
    let parents: BTreeSet<String> = places
        .windows(2)
        .filter(|pair| under(&pair[1].account.path, &pair[0].account.path))
        .map(|pair| pair[0].account.path.clone())
        .collect();
    let mut rows = Vec::new();
    let mut closed: Option<String> = None;
    for place in places {
        let path = place.account.path.clone();
        if closed.as_deref().is_none_or(|parent| !under(&path, parent)) {
            let is_open = opened.contains(&path);
            let fold = parents.contains(&path).then_some(is_open);
            closed = (fold == Some(false)).then_some(path);
            rows.push(Row::Node { place, fold });
        }
    }
    rows
}

/// Closes a dropdown when the reader presses or tabs out of its `control`,
/// returning what to hand the control's `focusout`.
///
/// Pressing rather than focus: Safari and Firefox focus neither a link nor a
/// button when it is pressed, so a pick would arrive as focus going nowhere
/// and close the panel out from under the click.
///
/// In an effect because the server has no window to listen to, and holding
/// the element itself because the callback runs outside the reactive
/// ownership tree, where the node ref reads back empty.
pub fn closes_on_leaving(
    control: NodeRef<Span>,
    close: impl Fn() + Clone + Send + Sync + 'static,
) -> impl Fn(ev::FocusEvent) + 'static {
    let on_press = close.clone();
    Effect::new(move |_| {
        // Each run registers its own listener, and its own cleanup takes that
        // one away: skipping the work when the control is unchanged would
        // leave the removal without its replacement.
        if let Some(control) = control.get() {
            let close = on_press.clone();
            let elsewhere = window_event_listener(ev::pointerdown, move |press| {
                let at = press.target().and_then(|t| t.dyn_into::<Node>().ok());
                if !control.contains(at.as_ref()) {
                    close();
                }
            });
            on_cleanup(move || elsewhere.remove());
        }
    });
    // Tabbing away is the other way out, and the only one focus can tell us
    // about: a press that lands nowhere focusable is not a departure.
    move |ev: ev::FocusEvent| {
        let into = ev.related_target().and_then(|target| target.dyn_into::<Node>().ok());
        if into.is_some_and(|node| {
            control.get_untracked().is_none_or(|control| !control.contains(Some(&node)))
        }) {
            close();
        }
    }
}

/// A form field holding an account's path, shown by its name. What the box
/// says and what the form submits never differ: leaving it with text that
/// names no account puts back the account it held.
#[component]
pub fn AccountBox(
    name: String,
    accounts: Accounts,
    #[prop(optional)] selected: String,
    #[prop(optional)] placeholder: &'static str,
) -> impl IntoView {
    let shown = accounts.name_of(&selected);
    let opened = RwSignal::new(accounts.opened_for(&selected));
    let path = RwSignal::new(selected);
    let text = RwSignal::new(shown.clone());
    let open = RwSignal::new(false);
    // Everything is listed until the reader types.
    let typing = RwSignal::new(false);
    // The new account's name, while the box asks what it goes under.
    let naming = RwSignal::new(None::<String>);
    let active = RwSignal::new(0usize);
    let control: NodeRef<Span> = NodeRef::new();
    let field: NodeRef<Input> = NodeRef::new();
    let list: NodeRef<Ul> = NodeRef::new();
    let create = ServerAction::<OpenAccount>::new();

    let listed = move || {
        let typed = if typing.get() { text.get() } else { String::new() };
        let asking = naming.get().is_some();
        let picking = if asking { accounts.opening_under() } else { accounts.all() };
        match typed.trim().is_empty() {
            true => tree(accounts.places(picking), &opened.get()),
            false => {
                let places = accounts.opening_under();
                let new = (!asking).then(|| NewAccount::read(&places, &path.get(), &typed));
                search(picking, &typed, new)
            }
        }
    };
    let fold = move |path: String, open: bool| {
        opened.update(|o| {
            if open {
                o.insert(path);
            } else {
                o.remove(&path);
            }
        });
    };
    let close = move || {
        open.set(false);
        typing.set(false);
        naming.set(None);
        active.set(0);
    };
    let pick = move |account: ChartAccount| {
        path.set(account.path);
        text.set(account.name);
        close();
    };
    let settle = move || {
        if open.get_untracked() {
            let typed = text.get_untracked();
            let typed = typed.trim();
            let named = || {
                let all = accounts.all();
                let mut named = all.into_iter().filter(|a| a.label == typed || a.name == typed);
                named.next().filter(|_| named.next().is_none())
            };
            let asking = naming.get_untracked().is_some();
            match (asking, typed.is_empty(), typing.get_untracked().then(named).flatten()) {
                (false, true, _) => path.set(String::new()),
                (false, false, Some(account)) => {
                    path.set(account.path);
                    text.set(account.name);
                }
                _ => text.set(accounts.name_of(&path.get_untracked())),
            }
            close();
        }
    };
    let chosen = move |account: ChartAccount| match naming.get_untracked() {
        Some(name) => {
            create.dispatch(OpenAccount { parent: account.path, name });
        }
        None => pick(account),
    };
    let activate = move |row: Row| match row {
        Row::New(NewAccount { name, under: Some(under) }) => {
            create.dispatch(OpenAccount { parent: under.path, name });
        }
        Row::New(NewAccount { name, under: None }) => {
            naming.set(Some(name));
            text.set(String::new());
            typing.set(true);
            active.set(0);
            create.clear();
        }
        Row::Found(account) | Row::Node { place: Place { account, pickable: true }, .. } => {
            chosen(account)
        }
        Row::Node { place: Place { account, pickable: false }, fold: open } => {
            fold(account.path, !open.unwrap_or(false));
        }
    };
    Effect::new(move |_| {
        if let Some(Ok(account)) = create.value().get() {
            accounts.opened.update(|a| a.push(account.clone()));
            pick(account);
        }
    });
    // Client only: what a reader typed lives in the element, where no attribute
    // reaches it.
    Effect::new(move |_| {
        if let Some(input) = field.get() {
            let showing = text.get();
            if input.value() != showing {
                input.set_value(&showing);
            }
        }
    });
    Effect::new(move |_| {
        let at = active.get();
        // Rows are all one height, so the list scrolls to the row and the page
        // stays where it is.
        if let Some(list) = list.get() {
            let row = list.scroll_height() / (list.child_element_count().max(1) as i32);
            let top = at as i32 * row;
            if top < list.scroll_top() {
                list.set_scroll_top(top);
            } else if top + row > list.scroll_top() + list.client_height() {
                list.set_scroll_top(top + row - list.client_height());
            }
        }
    });
    let left = closes_on_leaving(control, settle);

    let row_view = move |(i, row): (usize, Row)| {
        let (text, depth, folds) = match &row {
            Row::Found(a) => (a.name.clone(), 0, None),
            Row::Node { place, fold } => {
                let folds = fold.map(|open| (place.account.path.clone(), open));
                (place.account.label.clone(), place.account.depth, folds)
            }
            Row::New(new) => (new.to_string(), 0, None),
        };
        let has_fold = folds.is_some();
        let marker = folds.map(|(path, open)| {
            view! {
                <button
                    type="button"
                    class="fold"
                    tabindex="-1"
                    aria-label="口座目錄"
                    aria-expanded=open.to_string()
                    on:click=move |_| fold(path.clone(), !open)
                >
                    {if open { "▾" } else { "▸" }}
                </button>
            }
        });
        view! {
            <li style=format!("padding-left: {depth}rem") class:leaf=!has_fold>
                {marker}
                <button
                    type="button"
                    class="row"
                    tabindex="-1"
                    class:active=move || active.get() == i
                    on:click=move |_| activate(row.clone())
                >
                    {text}
                </button>
            </li>
        }
    };
    let panel = move || {
        open.get().then(|| {
            view! {
                // Pressing a row must not take the focus from the box.
                <div class="panel" on:mousedown=|ev| ev.prevent_default()>
                    {move || {
                        naming
                            .get()
                            .map(|name| {
                                view! { <p class="note">{format!("「{name}」開佇佗位？")}</p> }
                            })
                    }}
                    {move || {
                        create
                            .value()
                            .get()
                            .and_then(Result::err)
                            .map(|e| view! { <p class="note error">{crate::error::said(&e)}</p> })
                    }}
                    <ul node_ref=list>
                        {move || listed().into_iter().enumerate().map(row_view).collect_view()}
                    </ul>
                </div>
            }
            .into_any()
        })
    };
    view! {
        <span class="account-box" node_ref=control on:focusout=left>
            <input type="hidden" name=name value=move || path.get() />
            <input
                type="text"
                role="combobox"
                autocomplete="off"
                aria-expanded=move || open.get().to_string()
                placeholder=move || match naming.get() {
                    Some(_) => "揀上頂層",
                    None => placeholder,
                }
                node_ref=field
                value=shown
                on:focus=move |ev| {
                    event_target::<leptos::web_sys::HtmlInputElement>(&ev).select();
                    open.set(true);
                }
                on:click=move |_| open.set(true)
                on:input=move |ev| {
                    create.clear();
                    text.set(event_target_value(&ev));
                    typing.set(true);
                    active.set(0);
                    open.set(true);
                }
                on:keydown=move |ev| match ev.key().as_str() {
                    "ArrowDown" => {
                        ev.prevent_default();
                        open.set(true);
                        let last = listed().len().saturating_sub(1);
                        active.update(|a| *a = (*a + 1).min(last));
                    }
                    "ArrowUp" => {
                        ev.prevent_default();
                        active.update(|a| *a = a.saturating_sub(1));
                    }
                    // Across the tree; in the text, the arrows move the caret.
                    "ArrowRight" | "ArrowLeft" if open.get_untracked() => {
                        let at = listed().into_iter().nth(active.get_untracked());
                        if let Some(Row::Node { place, fold: Some(_) }) = at {
                            ev.prevent_default();
                            fold(place.account.path, ev.key() == "ArrowRight");
                        }
                    }
                    // In the list, Enter picks instead of submitting the form.
                    "Enter" if open.get_untracked() => {
                        ev.prevent_default();
                        if let Some(row) = listed().into_iter().nth(active.get_untracked()) {
                            activate(row);
                        }
                    }
                    "Escape" => settle(),
                    _ => {}
                }
            />
            {panel}
        </span>
    }
    .into_any()
}
