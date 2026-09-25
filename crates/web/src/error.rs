//! What a reader is told when a page cannot load or a change is refused.

use leptos::prelude::ServerFnError;

/// The server's own words. Anything else it says about the call itself is
/// the framework's English, which no page should show.
pub fn said(error: &ServerFnError) -> String {
    match error {
        ServerFnError::ServerError(message) => message.clone(),
        other => other.to_string(),
    }
}

pub fn load_failed(error: &ServerFnError) -> String { format!("讀取失敗：{}", said(error)) }
