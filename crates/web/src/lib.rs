//! The ledger's web UI: Leptos, rendered on the server and hydrated in the
//! browser. `cargo leptos build` builds both halves; the `ssr` feature is the
//! server, `hydrate` the wasm client.

#[cfg(feature = "ssr")]
pub mod account_tree;
pub mod accounts;
pub mod app;
pub mod balance_sheet;
#[cfg(feature = "ssr")]
pub mod entries;
pub mod error;
pub mod fold;
pub mod journal;
pub mod manual;
pub mod model;
#[cfg(feature = "ssr")]
pub mod queue;
#[cfg(feature = "ssr")]
pub mod rates;
pub mod review;
#[cfg(feature = "ssr")]
pub mod server;
#[cfg(feature = "ssr")]
pub mod sheet;
pub mod summary;
