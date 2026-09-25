//! The document shell, the layout and the routes.

use leptos::prelude::*;
use leptos_meta::{provide_meta_context, HashedStylesheet, MetaTags, Title};
use leptos_router::{
    components::{Route, Router, Routes, A},
    path,
};

use crate::{
    accounts::AccountsPage,
    balance_sheet::BalanceSheetPage,
    journal::JournalPage,
    manual::ManualPage,
    review::{Actions, EditPage, ReviewCount, ReviewPage},
    summary::SummaryPage,
};

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="zh-Hant-TW">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <meta name="color-scheme" content="light dark" />
                <AutoReload options=options.clone() />
                <HashedStylesheet options=options.clone() id="leptos" />
                <HydrationScripts options />
                <MetaTags />
            </head>
            <body>
                <App />
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    provide_context(Actions::new());
    view! {
        <Title formatter=|page: String| format!("{page} · 帳簿") />
        <Router>
            <header class="top">
                <nav>
                    <A href="/" exact=true>"總覽"</A>
                    <A href="/balance-sheet">"資產負債表"</A>
                    <A href="/journal">"日記帳"</A>
                    <A href="/review">"待確認" <ReviewCount /></A>
                    <A href="/entry">"記一筆"</A>
                    <A href="/accounts">"帳戶"</A>
                </nav>
            </header>
            <main>
                <Routes fallback=|| view! { <p class="note">"查無此頁"</p> }>
                    <Route path=path!("/") view=SummaryPage />
                    <Route path=path!("/balance-sheet") view=BalanceSheetPage />
                    <Route path=path!("/journal") view=JournalPage />
                    <Route path=path!("/review") view=ReviewPage />
                    <Route path=path!("/review/:id") view=EditPage />
                    <Route path=path!("/entry") view=ManualPage />
                    <Route path=path!("/accounts") view=AccountsPage />
                </Routes>
            </main>
        </Router>
    }
}

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(App);
}
