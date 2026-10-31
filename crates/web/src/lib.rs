//! The ledger's web UI: Leptos, rendered on the server and hydrated in the
//! browser. `cargo leptos build` builds both halves; the `ssr` feature is the
//! server, `hydrate` the wasm client.

pub mod account;
pub mod account_box;
pub mod app;
pub mod balance_sheet;
pub mod charts;
pub mod coverage;
#[cfg(feature = "ssr")]
pub mod detail;
#[cfg(feature = "ssr")]
pub mod entries;
pub mod error;
pub mod journal;
pub mod manual;
pub mod model;
pub mod plot;
#[cfg(feature = "ssr")]
pub mod queue;
#[cfg(feature = "ssr")]
pub mod rates;
pub mod review;
#[cfg(feature = "ssr")]
pub mod server;
#[cfg(feature = "ssr")]
pub mod sheet;
pub mod stored;
#[cfg(feature = "ssr")]
pub mod trends;
#[cfg(feature = "ssr")]
pub mod typed;
