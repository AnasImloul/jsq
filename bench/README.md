# Benchmarks

Reproducible comparison of `jsq` against [`jq`](https://jqlang.github.io/jq/) on the
same files and semantically equivalent queries. Results live in
[`../docs/BENCHMARKS.md`](../docs/BENCHMARKS.md).

## What's here

- `gen.py` — generates a deterministic `{"events":[…]}` file of a target size.
- `queries/qN.jsq` / `queries/qN.jq` — four equivalent query pairs of increasing
  complexity (see below).
- `run.sh` — runs the sweep, measuring median wall time (hyperfine) and peak resident
  memory (`/usr/bin/time -l`).

## The queries

| #  | Shape                              | jsq                                              |
|----|------------------------------------|--------------------------------------------------|
| q1 | filter + project                   | `where … select { … }`                           |
| q2 | filter + count                     | `where … aggregate { n: count() }`               |
| q3 | group-by single sum                | `aggregate { revenue: sum(…) } by region`        |
| q4 | group-by, three metrics, ordered   | `aggregate { sum, count, avg } by region order by` |

Each `.jq` file is the idiomatic, efficient jq equivalent (a `reduce` into an object
for the group-bys, not `group_by`, so jq isn't strawmanned). The pairs produce
identical output — verified before timing.

## Reproducing

Requires `jq`, [`hyperfine`](https://github.com/sharkdp/hyperfine), Python 3, and a
release build of jsq (`cargo build --release` in `../engine`).

```sh
# Generate inputs
python3 gen.py --target-mb 10   --out /tmp/ev_10mb.json
python3 gen.py --target-mb 100  --out /tmp/ev_100mb.json
python3 gen.py --target-mb 1024 --out /tmp/ev_1gb.json

# Run a sweep (prints a markdown table)
./run.sh bench /tmp/ev_100mb.json "100 MB" 5

# Run a single query once (for profiling / output inspection)
./run.sh exec jsq /tmp/ev_100mb.json queries/q3.jsq
./run.sh exec jq  /tmp/ev_100mb.json queries/q3.jq
```

`JSQ=/path/to/jsq ./run.sh …` overrides which binary is used.
