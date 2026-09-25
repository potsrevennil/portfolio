//! Fold-out trees: which groups are open, remembered per browser, and kept in
//! step with the `<details>` a reader toggles.

use std::collections::BTreeSet;

use leptos::{prelude::*, web_sys::HtmlDetailsElement};

/// The open groups, starting from `initial` and then from what this browser
/// last left open under `key`.
pub fn remembered(key: &'static str, initial: BTreeSet<String>) -> RwSignal<BTreeSet<String>> {
    let open = RwSignal::new(initial);
    // Client only: restore the open groups once, then save every change. The
    // `open` attribute is rendered on the server, and hydration adopts the
    // markup as it stands, so a restore has to land after it — the next frame.
    Effect::new(move |restored: Option<()>| {
        match restored {
            None => request_animation_frame(move || {
                if let Some(saved) = load(key) {
                    open.set(saved);
                }
            }),
            Some(()) => save(key, &open.read()),
        }
        open.track();
    });
    open
}

/// Whether the group at `path` is open, tracked for the `open` attribute.
pub fn is_open(path: String, open: RwSignal<BTreeSet<String>>) -> impl Fn() -> bool {
    move || open.with(|o| o.contains(&path))
}

/// Keeps the fold state in step with a `<details>` the reader just toggled.
pub fn on_toggle(
    path: String,
    open: RwSignal<BTreeSet<String>>,
) -> impl Fn(leptos::web_sys::Event) {
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

fn storage() -> Option<leptos::web_sys::Storage> { window().local_storage().ok().flatten() }

fn load(key: &str) -> Option<BTreeSet<String>> {
    let json = storage()?.get_item(key).ok()??;
    serde_json::from_str(&json).ok()
}

fn save(key: &str, open: &BTreeSet<String>) {
    if let (Some(storage), Ok(json)) = (storage(), serde_json::to_string(open)) {
        // Private windows may refuse; the fold state is a convenience.
        let _ = storage.set_item(key, &json);
    }
}
