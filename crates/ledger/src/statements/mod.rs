//! Bank and broker statement parsers, one module per institution.
//!
//! Each exports its own format into whatever shape that format actually has.
//! No shared `Statement` type yet — the second institution is what will show
//! which fields are genuinely common, and inventing it from a sample of one
//! would just be guessing.

pub mod cathay;
