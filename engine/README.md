# engine

The BigJSON query engine. A streaming JSON parser, offset-index builder, and SQL-shaped query evaluator — packaged as a Rust library, a C ABI, and the `jsq` command-line tool.

Used by:
- the `jsq` CLI in this crate
- the BigJSON macOS app (`../app/`) via the C ABI exposed in `include/engine.h`

The engine has no platform-specific code. It builds on macOS, Linux, and Windows; only the static-library linker glue used by the macOS app is platform-specific (that lives in `../scripts/build-engine.sh`).

## Build

```sh
cargo build --release           # library + jsq binary
cargo install --path .          # installs jsq to ~/.cargo/bin
cargo test                      # 100+ integration + unit tests
```

## Layout

```
src/
  document.rs            Streaming JSON parser + offset-record table.
  query/
    surface/             SQL-shaped surface language (parser, lowerer, formatter).
    evaluator/           Engine AST evaluator (walk, scan, aggregate, reducers).
    ast.rs               Engine-level query AST.
  render.rs              Output formatters (ndjson, json array, csv, tsv, table).
  ffi/                   C ABI surfaced to the macOS app.
  bin/jsq.rs             Command-line front-end.
tests/                   Integration tests against fixture documents.
examples/                Throwaway micro-benchmarks and probes.
include/engine.h         Hand-written C header for FFI consumers.
```

## Why a single-pass parser + record table?

For a one-shot CLI query, building the record table is more work than a pure streaming parser would do — strictly speaking. We do it anyway because:

1. The parser is fast enough that the record build still beats streaming-jq's per-token allocations by 5–6× on real queries.
2. Selective queries skip past most of the table during evaluation — `.dimensions[*].field` never re-walks `.series`.
3. Joins literally need the table (the `Lookup` op walks a hashmap built over candidate records).
4. The same code serves the interactive macOS app, where the parse cost is paid once and amortised over many queries in a session.
