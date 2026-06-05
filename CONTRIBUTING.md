# Contributing

Thanks for your interest in improving jsq. This is a small project, so the process
is light.

## Repo layout

- `engine/` — the Rust crate: query engine, parser, evaluator, FFI, and the `jsq`
  binary. Self-contained; builds on macOS, Linux, and Windows.
- `desktop/` — the Tauri + Svelte desktop app. A thin layer that links the engine
  crate directly.

Every semantic decision — query syntax, evaluation, output formatting — lives in
`engine/`. The app consumes results and adds no query logic of its own, so most
contributions land in `engine/`.

## Engine (Rust)

```sh
cd engine
cargo build --all-targets        # library + jsq binary
cargo test --all-targets         # unit + integration tests
cargo fmt --all                  # format before committing
cargo clippy --all-targets       # lint
```

The surface query language lives in `engine/src/query/surface/` (parser, lowerer,
formatter). The grammar vocabulary is defined once in `engine/src/query/grammar.rs`
and verified by `engine/tests/grammar_manifest.rs` — if you add or rename a keyword,
update the grammar table and that manifest test will keep everything in sync.

`engine/tests/query_surface.rs` has runnable examples covering every clause; it's the
best place to add a regression test for a language change.

## Desktop app (Tauri + Svelte)

```sh
cd desktop
npm install
npm run tauri dev                # run the app locally
npm run tauri build              # build a platform bundle
npm run check                    # type-check the Svelte frontend
```

The Tauri shell (`desktop/src-tauri/`) depends on the engine crate via a path
dependency, so `cargo` and a Node toolchain are the only out-of-band requirements.
On Linux you also need the webkit2gtk dev libraries (see `.github/workflows/ci.yml`
for the exact packages).

## Pull requests

- Keep changes focused; one logical change per PR.
- Run `cargo test`, `cargo fmt`, and `cargo clippy` before opening a PR. CI runs the
  engine tests on Linux and a desktop (Tauri) build.
- Add or update tests for behavior changes, especially anything touching the query
  language.
- Update [docs/QUERY.md](docs/QUERY.md) and [CHANGELOG.md](CHANGELOG.md) when you
  change user-facing syntax or behavior.

## Reporting bugs

Open an issue with the `jsq` version (`jsq --version`), the query, and a minimal
JSON input that reproduces the problem. For a query that parses but behaves
unexpectedly, the output of `jsq --explain <FILE> '<QUERY>'` is useful.
