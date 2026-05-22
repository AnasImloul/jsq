# BigJSON

Tools for opening, exploring, and querying very large JSON files (1+ GB) with a SQL-shaped query language. Two front-ends share one engine:

- **`jsq`** — a cross-platform command-line tool (Rust). Pipes one JSON value per line so the output composes with `jq`, `wc`, `head`, etc.
- **BigJSON.app** — a native macOS UI (SwiftUI) for interactive exploration: streaming open, virtual rows, filter-as-you-type, exports.

Both delegate every byte of output and every semantic decision to the same Rust engine — adding a feature is a one-place change.

## Layout

```
engine/    Rust crate. The query engine, parser, evaluator, FFI, and `jsq` binary.
           Self-contained; can be built / installed / published independently.
app/       Swift / Xcode project for the macOS UI. Depends on engine/ via the
           static library + FFI header that engine's build phase produces.
scripts/   Shared build helpers (build-engine.sh, release.sh).
```

## Quick start

```sh
# CLI
cd engine && cargo install --path .
jsq path/to/file.json 'from .events[*] as e count'

# macOS app
open app/BigJSON.xcodeproj          # then ⌘R, or:
scripts/release.sh                  # builds a signed .dmg in ./build
```

## Query language

SQL-shaped surface syntax (`from … as … (join …)? where … aggregate …`). See `engine/src/query/surface/` for the parser and `engine/tests/query_surface.rs` for runnable examples covering every clause.
