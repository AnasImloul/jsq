//! Big-JSON engine. Streaming parser + offset index for very large JSON
//! files, exposed to Swift through a small C ABI.
//!
//! # Index model: hybrid container records
//!
//! The engine indexes a JSON file into a flat, pre-order array of 32-byte
//! `NodeRecord`s. Records are emitted only for **containers** (`Object`,
//! `Array`) and **strings whose source span ≥ `FAT_STRING_THRESHOLD`**;
//! smaller primitives (numbers, booleans, nulls, short strings) live
//! only in the source mmap and are surfaced on demand by source-scanning
//! their parent.
//!
//! Two key invariants the rest of the engine depends on:
//!
//! 1. **Pre-order layout** — for any record at index `k` with
//!    `subtree_size = n`, the records `[k+1 .. k+n]` are exactly the
//!    descendants of `k` in source order. Sibling pointers are derived
//!    via `id + subtree_size`; no explicit `next_sibling` field.
//!
//! 2. **Skippable + primitive interleaving** — when iterating a
//!    container's children, primitives are reconstructed by walking the
//!    container's source bytes in lockstep with the skippable record
//!    chain. The shared scanner lives in [`source_scan`]; consumers
//!    are [`query::evaluator`] and [`ffi`].
//!
//! In debug builds, [`document::Document::verify_subtree_invariants`]
//! is run after every parse to catch any layout violation at the source.

pub(crate) mod container_scan;
pub mod document;
pub mod error;
pub mod ffi;
pub mod parser;
pub mod path;
pub mod progress;
pub mod query;
pub mod render;
pub mod sidecar;
pub mod source_scan;
