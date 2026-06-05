# jsq

[![CI](https://github.com/AnasImloul/jsq/actions/workflows/ci.yml/badge.svg)](https://github.com/AnasImloul/jsq/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/AnasImloul/jsq)](https://github.com/AnasImloul/jsq/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Open, explore, and query very large JSON files (1+ GB) with a **SQL-shaped query
language**. Where `jq` is a per-token stream processor, `jsq` runs one declarative
query — `from … where … aggregate … order by …` — in a single streaming pass at
GB-scale, and prints NDJSON so its output still composes with `jq`, `head`, `wc`, etc.

Two front-ends share one engine:

- **`jsq`** — a cross-platform command-line tool (Rust). Pipes one JSON value per line so the output composes with `jq`, `wc`, `head`, etc.
- **BigJSON** — a cross-platform desktop UI (Tauri + Svelte) for interactive exploration: streaming open, virtual rows, filter-as-you-type, exports.

Both delegate every byte of output and every semantic decision to the same Rust engine — adding a feature is a one-place change.

## Layout

```
engine/    Rust crate. The query engine, parser, evaluator, FFI, and `jsq` binary.
           Self-contained; can be built / installed / published independently.
desktop/   Tauri + Svelte desktop UI. Depends on engine/ as a Rust crate
           (src-tauri links it directly — no FFI).
```

## Install

```sh
# CLI — Homebrew (macOS & Linux, arm64 + x86_64)
brew tap AnasImloul/homebrew-tap
brew install jsq

# CLI — crates.io (needs the Rust toolchain)
cargo install jsql        # the crate is `jsql`; it installs the `jsq` command

# CLI — from source
cd engine && cargo install --path .
```

The desktop app ships as platform bundles on the [releases page](https://github.com/AnasImloul/jsq/releases) — `.dmg` for macOS (Apple Silicon + Intel), `.msi`/`.exe` for Windows, and `.AppImage`/`.deb` for Linux. The bundles are unsigned, so the OS shows an "unidentified developer" warning on first launch (on macOS, right-click → Open, or `xattr -d com.apple.quarantine /Applications/BigJSON.app`).

## Quick start

```sh
# Count the elements of an array
jsq orders.json 'from .orders[] as o aggregate { n: count() }'
# → 3

# Filter, then reshape each row
jsq orders.json 'from .orders[] as o
  where o.status == "paid"
  select { id: o.id, total: o.total }'

# Group-and-sum, sorted — then keep only the top 5 with jq's friends
jsq orders.json 'from .orders[] as o
  aggregate { revenue: sum(o.total) } by o.region
  order by .revenue desc' | head -5

# Read from stdin
cat orders.json | jsq - 'from .orders[] as o where o.total > 100'
```

Every query emits **NDJSON** (one JSON value per line), so pipe into `jq`, `head`,
`wc`, etc. for post-processing.

### Desktop app

```sh
cd desktop
npm install
npm run tauri dev                   # run the app locally, or:
npm run tauri build                 # build a platform bundle
```

## Usage

```
jsq [OPTIONS] [FILE] [QUERY]
```

| Flag | Description |
|------|-------------|
| `-n, --limit <N>`        | Cap result rows (default unlimited). |
| `-p, --param <NAME=VAL>` | Bind a `$name` query parameter (repeatable). |
| `-s, --stats`            | Print timing / scan stats to stderr after results. |
| `-S, --stats-only`       | Print only stats, suppress the result stream. |
| `-e, --explain`          | Print the lowered engine AST instead of running. |
| `--format-only`          | Pretty-print / canonicalize a query (no file needed). |

Pass `-` as FILE to read from stdin. Run `jsq --help` for the full reference.

## Query language

A complete language reference — clause pipeline, paths, operators, aggregation,
joins, subqueries, and a guide to translating Python/JS/SQL logic into jsq — lives in
**[docs/QUERY.md](docs/QUERY.md)**.

The parser, lowerer, and formatter are in `engine/src/query/surface/`, and
`engine/tests/query_surface.rs` has runnable examples covering every clause.

## Benchmarks

`jsq` streams in a single memory-mapped pass; `jq` and `jaq` (jq's Rust reimplementation)
parse the whole document onto the heap. So jsq's memory stays flat as files grow while
theirs tracks the file size — and the time gap widens with it. Same group-by-and-sum
query, three file sizes (wall time / RAM):

| File   | jsq | jq | jaq | jsq RAM | jq RAM | jaq RAM |
|--------|----:|---:|----:|--------:|-------:|--------:|
| 10 MB  |  0.26s |  0.45s |  0.24s | 30 MiB | 167 MiB | 154 MiB |
| 100 MB |  0.60s |  3.81s |  1.77s | 33 MiB | 1.7 GB  | 1.5 GB  |
| 1 GB   |  7.00s | 68.98s | 28.37s | 34 MiB |  17 GB  |  15 GB  |

"RAM" is the real memory the process owns — the figure Activity Monitor shows. At 1 GB
jsq answers in ~7s holding **34 MiB**; jaq, despite being a fast Rust rewrite, still needs
**~15 GB** because it parses everything. **The memory win is the streaming architecture,
not the language** — jsq beats jaq on RAM by the same ~450× it beats jq.

See **[docs/BENCHMARKS.md](docs/BENCHMARKS.md)** for the full methodology, all four query
shapes, and both memory metrics (jq/jaq are faster on small files — the reference is
honest about where each tool wins).
