//! The document shell, the layout and the routes.

use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet, Title};
use leptos_router::{
    components::{Route, Router, Routes, A},
    path,
};

use crate::{balance_sheet::BalanceSheetPage, summary::SummaryPage};

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="zh-Hant-TW">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <meta name="color-scheme" content="light dark" />
                <AutoReload options=options.clone() />
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
    view! {
        <Stylesheet id="leptos" href="/pkg/web.css" />
        <Title formatter=|page: String| format!("{page} · 帳簿") />
        <Router>
            <header class="top">
                <nav>
                    <A href="/" exact=true>"總覽"</A>
                    <A href="/balance-sheet">"資產負債表"</A>
                </nav>
            </header>
            <main>
                <Routes fallback=|| view! { <p class="note">"查無此頁"</p> }>
                    <Route path=path!("/") view=SummaryPage />
                    <Route path=path!("/balance-sheet") view=BalanceSheetPage />
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
