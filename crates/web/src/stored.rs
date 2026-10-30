//! What a page remembers between visits, per browser, in localStorage.
//! Private windows may refuse; it is a convenience, never state.

use leptos::prelude::*;
use serde::{de::DeserializeOwned, Serialize};

/// Restores `signal` from `key` once the page is live, then saves every
/// change. Markup rendered on the server is adopted as it stands when the
/// page hydrates, so the restore lands the frame after.
pub fn remember<T>(key: &'static str, signal: RwSignal<T>)
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    Effect::new(move |restored: Option<()>| {
        match restored {
            None => request_animation_frame(move || {
                if let Some(saved) = load(key) {
                    signal.set(saved);
                }
            }),
            Some(()) => save(key, &*signal.read()),
        }
        signal.track();
    });
}

fn storage() -> Option<leptos::web_sys::Storage> { window().local_storage().ok().flatten() }

fn load<T: DeserializeOwned>(key: &str) -> Option<T> {
    let json = storage()?.get_item(key).ok()??;
    serde_json::from_str(&json).ok()
}

fn save<T: Serialize>(key: &str, value: &T) {
    if let (Some(storage), Ok(json)) = (storage(), serde_json::to_string(value)) {
        let _ = storage.set_item(key, &json);
    }
}
