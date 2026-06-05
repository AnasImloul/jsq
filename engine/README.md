# engine

The jsq query engine. A streaming JSON parser, offset-index builder, and SQL-shaped query evaluator — packaged as a Rust library, a C ABI, and the `jsq` command-line tool.

Used by:
- the `jsq` CLI in this crate
- the BigJSON desktop app (`../desktop/`), which links this crate directly as a Rust path dependency and drives the interactive UI through the `ffi` module's functions (called as ordinary Rust)

The engine has no platform-specific code. It builds on macOS, Linux, and Windows.

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
  ffi/                   Flat function surface the desktop app's Rust bridge calls.
  bin/jsq.rs             Command-line front-end.
tests/                   Integration tests against fixture documents.
examples/                Throwaway micro-benchmarks and probes.
```

## Why a single-pass parser + record table?

For a one-shot CLI query, building the record table is more work than a pure streaming parser would do — strictly speaking. We do it anyway because:

1. The parser is fast enough that the record build still beats streaming-jq's per-token allocations by 5–6× on real queries.
2. Selective queries skip past most of the table during evaluation — `.books[].field` never re-walks `.loans`.
3. Joins literally need the table (the `Lookup` op walks a hashmap built over candidate records).
4. The same code serves the interactive desktop app, where the parse cost is paid once and amortised over many queries in a session.
