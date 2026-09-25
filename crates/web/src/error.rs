//! What a reader is told when a page cannot load.

use leptos::prelude::ServerFnError;

/// The server's own words. Anything else it says about the call itself is
/// the framework's English, which no page should show.
pub fn load_failed(error: &ServerFnError) -> String {
    let said = match error {
        ServerFnError::ServerError(message) => message.clone(),
        other => other.to_string(),
    };
    format!("讀取失敗：{said}")
}
