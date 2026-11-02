//! The pure core of yAGna: data types and math that own no world.
//!
//! This crate has no platform conditionals, no file access, no clock
//! reads, no network, no UI toolkit and no database — a gate test greps the
//! crate's own sources so that freedom cannot rot. `now` is always a
//! parameter, so every computation here is deterministic.
//!
//! Items: `model` — the transfer types every crate speaks; `health` — the
//! 12-point repository health index; `contrast` — the WCAG relative
//! luminance and contrast ratio of two colours; `markdown` — README image
//! extraction, badge detection, the offline rewrite, the excerpt, the
//! HTML attribute scanner; `mdrender` — the README renderer (markdown
//! in, blocks out, the asset resolver's contract); `treefmt` — the tree
//! listing rendered as box-drawing text; `fuzzy` — the memory-only fuzzy
//! index over the catalog; `digest` — the gitingest-format digest
//! formatter and its spec filter; `query` — the deep search's query AST
//! and parser; `export` — the export file's shape (the stars
//! exporter's row plus the yagna extension). The core-freedom gate
//! lives in `tests/gates.rs`; the markdown fixture corpus in
//! `tests/markdown_fixtures.rs`; the digest golden fixtures in
//! `tests/digest_fixtures/`.
//! Depends on: serde/serde_json (the transfer format), chrono (instants
//! and calendar math), blake3 (content-hash asset ids), nucleo (the
//! fuzzy matcher), globset (pure glob matching).
//! Called by: the store, forge and app crates.

pub mod contrast;
pub mod digest;
pub mod export;
pub mod fit;
pub mod fuzzy;
pub mod health;
pub mod markdown;
pub mod mdrender;
pub mod model;
pub mod query;
pub mod treefmt;
