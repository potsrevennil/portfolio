//! What a reader is told when a page cannot load or a change is refused.

use leptos::prelude::*;

#[component]
pub fn LoadFailed(error: ServerFnError) -> impl IntoView {
    view! { <p class="note error">{format!("讀取失敗：{}", said(&error))}</p> }
}

/// The server's own message where it sent one; a failure before that — no
/// answer, or one that would not decode — keeps the framework's English.
pub fn said(error: &ServerFnError) -> String {
    match error {
        ServerFnError::ServerError(message) => message.clone(),
        other => other.to_string(),
    }
}
