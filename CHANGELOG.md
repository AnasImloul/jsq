# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.2.0] - 2026-06-05

### Changed
- Replaced the macOS-only SwiftUI app with a cross-platform desktop app
  (Tauri + Svelte). Releases now ship desktop bundles for macOS (Apple Silicon +
  Intel), Windows, and Linux.

### Removed
- The Swift/Xcode app and its C-ABI build glue (static library, hand-written
  `engine.h` header, and the unused FFI functions only it consumed).

## [1.1.0] - 2026-06-02

### Added
- `distinct by KEY[, KEY]...` — dedupe the stream on a key tuple (emitting the
  first whole row per distinct key), the equivalent of SQL `DISTINCT ON`. Bare
  `distinct` (whole-row dedupe) is unchanged.

### Fixed
- Composite group-by (`aggregate { … } by k1, k2`) now emits one result column per
  key (`{"region": …, "tier": …, …}`) instead of a single column holding the keys
  joined by a U+001F separator.

## [1.0.0] - 2026-06-02

First public release.

### Added
- `jsq` — a streaming, single-pass CLI for querying very large (1+ GB) JSON files
  with a SQL-shaped query language. Emits NDJSON so it composes with `jq`, `head`,
  `wc`, etc.
- Query language: `from`/`join`/`unnest`/`where`/`let`/`distinct`/`aggregate`/
  `collect by`/`having`/`select`/`order by`/`limit`, path grammar (`[]`, `[N]`,
  `["key"]`, `.**`, field-sets), function-call reducers (`count`, `sum`, `avg`,
  `min`, `max`), item-level `where`, `??` defaults, `if()`, scalar functions, and
  correlated subqueries. See [docs/QUERY.md](docs/QUERY.md).
- CLI flags: `--limit`, `--param`, `--stats`, `--stats-only`, `--explain`,
  `--format-only`, plus stdin via `-`.
- BigJSON.app — a native macOS UI over the same engine (streaming open, virtual
  rows, filter-as-you-type, exports).
- Distribution: prebuilt CLI binaries for macOS and Linux (arm64 + x86_64) via a
  Homebrew tap, and a `.dmg` for the macOS app.

[Unreleased]: https://github.com/AnasImloul/jsq/compare/v1.1.0...HEAD
[1.1.0]: https://github.com/AnasImloul/jsq/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/AnasImloul/jsq/releases/tag/v1.0.0
