//! 對帳進度: how current each source is. A source is checked only as far as
//! the institution's own figure (or a count) vouches for it; records imported
//! after that are shown apart, so a current-looking balance never passes for
//! a checked one.

use std::fmt;

use leptos::prelude::*;
use leptos_meta::Title;
use serde::{Deserialize, Serialize};

use crate::{error::LoadFailed, model::JournalQuery};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    pub today: String,
    pub sources: Vec<SourceRow>,
    /// Accounts holding a balance no configured source covers.
    pub unsourced: Vec<Unsourced>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Checked,
    /// Records after the last figure: imported, not checked.
    Unchecked,
    ToDownload,
    ToCount,
    Overdue,
}

impl Status {
    fn class(self) -> &'static str {
        match self {
            Status::Checked => "checked",
            Status::Unchecked => "unchecked",
            Status::ToDownload | Status::ToCount => "due",
            Status::Overdue => "overdue",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Status::Checked => "已對帳",
            Status::Unchecked => "已匯入未對帳",
            Status::ToDownload => "待下載",
            Status::ToCount => "待點算",
            Status::Overdue => "逾期",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRow {
    pub label: String,
    pub account: String,
    pub cadence: String,
    pub status: Status,
    /// 已對帳至: the last day an outside figure vouches for, across every
    /// commodity the source still holds.
    pub checked: Option<String>,
    /// Each commodity's own day, where they differ.
    pub per_commodity: Vec<String>,
    /// 落後: days from `checked` to today.
    pub behind_days: Option<i64>,
    /// 已匯入至: the newest record.
    pub imported: Option<String>,
    /// Records no figure has checked yet.
    pub tail: Option<Tail>,
    pub gaps: Vec<Gap>,
    pub missing: Option<Missing>,
    /// What `raw/manifest.csv` holds for the source.
    pub files: Option<Files>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tail {
    pub count: usize,
    pub first: String,
    pub last: String,
    /// The journal from its first day, where the records are postings.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    pub commodity: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Missing {
    /// The first figure missed after the last one; `None` when there has
    /// never been one.
    pub from: Option<String>,
    /// The newest figure due.
    pub through: Option<String>,
    /// A count is missing, not a statement.
    pub count: bool,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Files {
    pub count: usize,
    pub latest: String,
    /// The newest download date the manifest records, once it has the column.
    pub downloaded: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unsourced {
    pub label: String,
    pub account: String,
    pub imported: Option<String>,
}

#[server]
pub async fn load_coverage() -> Result<Coverage, ServerFnError> {
    let pool = expect_context::<db::SqlitePool>();
    let config = expect_context::<crate::progress::Config>();
    let today = chrono::Local::now().date_naive();
    crate::progress::load(&pool, &config, today)
        .await
        .map_err(|e| ServerFnError::new(format!("{e:#}")))
}

#[component]
pub fn CoveragePage() -> impl IntoView {
    let coverage = Resource::new(|| (), |_| load_coverage());
    view! {
        <Title text="對帳進度" />
        <Suspense fallback=|| view! { <p class="note">"載入中…"</p> }>
            {move || Suspend::new(async move {
                match coverage.await {
                    Ok(coverage) => view! { <CoverageView coverage /> }.into_any(),
                    Err(e) => view! { <LoadFailed error=e /> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
pub fn CoverageView(coverage: Coverage) -> impl IntoView {
    let empty = coverage.sources.is_empty();
    view! {
        <div class="coverage">
            <p class="as-of">{coverage.today}</p>
            <Show when=move || empty>
                <p class="note">"mapping.toml 猶未設定 [[sources]]。"</p>
            </Show>
            <table class="sources">
                <thead>
                    <tr>
                        <th>"來源"</th>
                        <th>"狀態"</th>
                        <th>"已對帳至"</th>
                        <th>"落後"</th>
                        <th>"已匯入至"</th>
                        <th>"未對帳"</th>
                        <th>"缺口"</th>
                        <th>"欠啥物"</th>
                    </tr>
                </thead>
                <tbody>
                    {coverage.sources.into_iter().map(source_row).collect_view()}
                </tbody>
            </table>
            {(!coverage.unsourced.is_empty()).then(|| unsourced(coverage.unsourced))}
        </div>
    }
}

fn source_row(s: SourceRow) -> AnyView {
    let journal = format!("/journal{}", JournalQuery::account(&s.account));
    // Hydration markers keep a cell from ever being :empty.
    let none = (
        s.behind_days.is_none(),
        s.imported.is_none(),
        s.tail.is_none(),
        s.gaps.is_empty(),
        s.missing.is_none() && s.files.is_none(),
    );
    let per_commodity = (!s.per_commodity.is_empty())
        .then(|| view! { <span class="detail">{s.per_commodity.join(" · ")}</span> });
    view! {
        <tr class=format!("source {}", s.status.class())>
            <th scope="row" data-label="來源">
                <a href=journal>{s.label}</a>
                <span class="detail">{s.cadence}</span>
            </th>
            <td data-label="狀態">
                <span class=format!("badge status {}", s.status.class())>{s.status.to_string()}</span>
            </td>
            <td data-label="已對帳至" class="checked">
                {s.checked.unwrap_or_else(|| "從未".into())}
                {per_commodity}
            </td>
            <td data-label="落後" class:none=none.0>{s.behind_days.map(|d| format!("{d} 日"))}</td>
            <td data-label="已匯入至" class:none=none.1>{s.imported}</td>
            <td data-label="未對帳" class="tail" class:none=none.2>{s.tail.map(tail)}</td>
            <td data-label="缺口" class:none=none.3>
                {s.gaps
                    .into_iter()
                    .map(|g| view! { <span class="gap">{format!("{} {} → {}", g.commodity, g.from, g.to)}</span> })
                    .collect_view()}
            </td>
            <td data-label="欠啥物" class:none=none.4>
                {s.missing.map(missing)}
                {s.files.map(files)}
            </td>
        </tr>
    }.into_any()
}

fn tail(t: Tail) -> impl IntoView {
    let text = match t.first == t.last {
        true => format!("{} 筆，{}", t.count, t.first),
        false => format!("{} 筆，{} → {}", t.count, t.first, t.last),
    };
    match t.link {
        Some(link) => view! { <a href=link>{text}</a> }.into_any(),
        None => view! { <span>{text}</span> }.into_any(),
    }
}

fn missing(m: Missing) -> AnyView {
    let what = match (m.count, m.from, m.through) {
        (true, Some(from), _) => format!("{from} 就該點算"),
        (true, None, _) => "猶未點算過".to_string(),
        (false, Some(from), Some(through)) if from != through => {
            format!("{from} → {through} 的對帳單")
        }
        (false, _, Some(through)) => format!("{through} 的對帳單"),
        (false, _, None) => "猶無對帳單".to_string(),
    };
    view! {
        <span class="missing">
            {what}
            {m.url.map(|url| view! { " " <a href=url rel="noreferrer" target="_blank">"去下載"</a> })}
        </span>
    }.into_any()
}

fn files(f: Files) -> AnyView {
    let name = f.latest.rsplit('/').next().unwrap_or(&f.latest);
    let text = match &f.downloaded {
        Some(d) => format!("{} 個檔，上新 {name}，{d} 下載", f.count),
        None => format!("{} 個檔，上新 {name}", f.count),
    };
    view! { <span class="detail" title=f.latest.clone()>{text}</span> }.into_any()
}

fn unsourced(accounts: Vec<Unsourced>) -> AnyView {
    view! {
        <section class="unsourced">
            <h2>"無設定來源的帳戶"</h2>
            <p class="note">"有餘額，毋過 [[sources]] 無這个帳戶：無人替伊對帳。"</p>
            <ul>
                {accounts
                    .into_iter()
                    .map(|a| {
                        let journal = format!("/journal{}", JournalQuery::account(&a.account));
                        view! {
                            <li>
                                <a href=journal>{a.label}</a>
                                <span class="detail">{a.imported.map(|d| format!("記到 {d}"))}</span>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </section>
    }.into_any()
}
