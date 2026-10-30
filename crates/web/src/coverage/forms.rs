//! The forms a bar opens: its statement schedule, and a balance confirmed by
//! hand.

use std::{str::FromStr, sync::Arc};

use ledger_types::{
    cadence::{CadenceKind, CadenceSpec},
    currency::Currency,
};
use leptos::prelude::*;

use super::{actions::CoverageActions, model::AccountRow};
use crate::review::ActionError;

/// Which form a bar has open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    Settings,
    Confirm,
}

/// The account and form that are open, if any.
pub(super) type Editing = RwSignal<Option<(String, Panel)>>;

/// Sets `signal` to what a select now holds.
fn picked<T: FromStr + Send + Sync + 'static>(signal: RwSignal<T>) -> impl Fn(leptos::ev::Event) {
    move |ev| {
        if let Ok(value) = event_target_value(&ev).parse() {
            signal.set(value);
        }
    }
}

fn cancel(editing: Editing) -> impl IntoView {
    view! { <button type="button" class="link" on:click=move |_| editing.set(None)>"取消"</button> }
}

/// The account's bank and statement schedule, saved to the database.
#[component]
pub(super) fn Settings(
    row: Arc<AccountRow>,
    editing: Editing,
    banks: Arc<[String]>,
) -> impl IntoView {
    let save = expect_context::<CoverageActions>().save;
    let CadenceSpec { kind: chosen, month, day, every } = CadenceSpec::from(row.cadence);
    let kind = RwSignal::new(chosen);
    let list = format!("banks-{}", row.account);
    let days = move || {
        (1..=31u32)
            .map(|d| {
                let text = match d {
                    31 => "月底".to_string(),
                    d => format!("{d} 號"),
                };
                view! { <option value=d.to_string() selected=day.unwrap_or(31) == d>{text}</option> }
            })
            .collect_view()
    };
    let kinds = [
        (CadenceKind::Never, "無排（毋提醒）"),
        (CadenceKind::Monthly, "逐月"),
        (CadenceKind::Quarterly, "逐季"),
        (CadenceKind::Yearly, "逐年"),
        (CadenceKind::Count, "每幾工對一擺"),
        (CadenceKind::Instalment, "分期付款"),
        (CadenceKind::Payment, "逐月還款"),
    ];
    view! {
        <div class="settings">
            <ActionForm action=save>
                <input type="hidden" name="account" value=row.account.clone() />
                <label>
                    "銀行 " <input type="text" name="bank" value=row.bank.clone() list=list.clone() required />
                    <datalist id=list>
                        {banks.iter().cloned().map(|b| view! { <option value=b></option> }).collect_view()}
                    </datalist>
                </label>
                <label>
                    "數單 "
                    <select name="kind" on:change=picked(kind)>
                        {kinds
                            .into_iter()
                            .map(|(value, text)| {
                                view! { <option value=value.to_string() selected=chosen == value>{text}</option> }
                            })
                            .collect_view()}
                    </select>
                </label>
                <Show when=move || kind.get() == CadenceKind::Yearly>
                    <label>
                        "月份 "
                        <select name="month">
                            {(1..=12u32)
                                .map(|m| view! { <option value=m.to_string() selected=month.unwrap_or(12) == m>{format!("{m} 月")}</option> })
                                .collect_view()}
                        </select>
                    </label>
                </Show>
                <Show when=move || kind.get().takes_day()>
                    <label>"結數日 " <select name="day">{days()}</select></label>
                </Show>
                <Show when=move || kind.get() == CadenceKind::Count>
                    <label>
                        "每 " <input type="number" name="every" min="1" value=every.unwrap_or(30).to_string() /> " 工"
                    </label>
                </Show>
                <label>
                    "寬限 " <input type="number" name="grace" min="0" value=row.grace.to_string() /> " 日"
                </label>
                <label title="銀行無保留這日進前的記錄">
                    "對帳起點 "
                    <input
                        type="date"
                        name="since"
                        value=row.since.as_ref().map(|m| m.date.clone()).unwrap_or_default()
                    />
                </label>
                <button type="submit">"儲存"</button>
                {cancel(editing)}
            </ActionForm>
            <ActionError result=save.value() />
        </div>
    }
}

/// A balance confirmed by hand: the day, and the figure the bank (or a count)
/// shows, prefilled with the ledger's.
#[component]
pub(super) fn Confirm(row: Arc<AccountRow>, today: String, editing: Editing) -> impl IntoView {
    let confirm = expect_context::<CoverageActions>().confirm;
    let currency = RwSignal::new(row.balances.first().map_or(Currency::TWD, |b| b.currency));
    let amount = {
        let row = row.clone();
        move || {
            let chosen = currency.get();
            row.balances
                .iter()
                .find(|b| b.currency == chosen)
                .map_or("0".into(), |b| b.amount.clone())
        }
    };
    view! {
        <div class="settings">
            <ActionForm action=confirm>
                <input type="hidden" name="account" value=row.account.clone() />
                <label>"到 " <input type="date" name="date" value=today required /> " 為止"</label>
                <label>
                    "餘額 " <input type="text" inputmode="decimal" name="amount" prop:value=amount required />
                </label>
                <label>
                    <select name="currency" on:change=picked(currency)>
                        {row
                            .balances
                            .iter()
                            .map(|b| view! { <option value=b.currency.to_string()>{b.currency.to_string()}</option> })
                            .collect_view()}
                        {row.balances.is_empty().then(|| view! { <option value="TWD">"TWD"</option> })}
                    </select>
                </label>
                <label>
                    <input type="checkbox" name="correct" value="1" />
                    "若無仝，記一筆差額校對（入待確認）"
                </label>
                <button type="submit">"對過矣"</button>
                {cancel(editing)}
            </ActionForm>
            <p class="note">"填銀行 app、Splitwise 抑是點算的實際數目。"</p>
            <ActionError result=confirm.value() />
        </div>
    }
}
