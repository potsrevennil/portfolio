//! The ledger's web UI: Leptos, rendered on the server and hydrated in the
//! browser. `cargo leptos build` builds both halves; the `ssr` feature is the
//! server, `hydrate` the wasm client.

pub mod app;
pub mod balance_sheet;
pub mod model;
#[cfg(feature = "ssr")]
pub mod server;
#[cfg(feature = "ssr")]
pub mod sheet;
