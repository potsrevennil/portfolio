//! Types both the server and the browser build use. Nothing here may depend on
//! SQLite, the network or the filesystem: this crate compiles to wasm.

pub mod assertion;
pub mod currency;

pub use currency::Currency;
