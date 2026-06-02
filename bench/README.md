# Benchmarks

Reproducible comparison of `jsq` against [`jq`](https://jqlang.github.io/jq/) and
[`jaq`](https://github.com/01mf02/jaq) (jq's Rust reimplementation) on the same files and
semantically equivalent queries. Results live in
[`../docs/BENCHMARKS.md`](../docs/BENCHMARKS.md).

## What's here

- `gen.py` — generates a deterministic `{"events":[…], "users":[…], "regions":[…]}` file
  of a target size: one large `events` fact array (grows with the target) plus a 10k-row
  `users` and 5-row `regions` dimension (byte-identical across sizes) for the joins.
- `queries/qN.jsq` / `queries/qN.jq` — nine equivalent query forms of increasing
  complexity (see below). jq and jaq run the same `.jq` filter.
- `run.sh` — runs the sweep, measuring median wall time (hyperfine) and peak memory —
  both RAM (`phys_footprint`) and RSS (`/usr/bin/time -l`) — for all three tools.

## The queries

| #  | Shape                                       | jsq                                                       |
|----|---------------------------------------------|-----------------------------------------------------------|
| q1 | filter + project                            | `where … select { … }`                                    |
| q2 | filter + count                              | `where … aggregate { n: count() }`                        |
| q3 | group-by single sum                         | `aggregate { revenue: sum(…) } by region`                 |
| q4 | group-by, three metrics, ordered            | `aggregate { sum, count, avg } by region order by`        |
| q5 | inner join + group-by                       | `join .users[] as u on … aggregate { … } by u.tier`       |
| q6 | chained two-hop join + group-by             | `join users … join regions … aggregate { … } by r.country`|
| q7 | unnest array + group-by                     | `unnest e.items as it aggregate { … } by it.sku`          |
| q8 | join + unnest + filter + multi-metric + order | `join … unnest … where … aggregate { … } by u.tier order by` |
| q9 | group-by + having (post-aggregate filter)   | `aggregate { … } by e.region having .revenue > …`         |

Each `.jq` file is the idiomatic, efficient jq equivalent — a `reduce` into an object for
every group-by (never `group_by`), and a hand-built index (`reduce … as $row`) for every
join — so neither jq nor jaq is strawmanned. All three tools produce identical output —
verified before timing.

## Reproducing

Requires `jq`, `jaq`, [`hyperfine`](https://github.com/sharkdp/hyperfine), Python 3, and
a release build of jsq (`cargo build --release` in `../engine`).

```sh
# Generate inputs
python3 gen.py --target-mb 10   --out /tmp/ev_10mb.json
python3 gen.py --target-mb 100  --out /tmp/ev_100mb.json
python3 gen.py --target-mb 1024 --out /tmp/ev_1gb.json

# Run a sweep (prints the time + memory markdown tables)
./run.sh bench /tmp/ev_100mb.json "100 MB" 5

# Run a single query once (for profiling / output inspection)
./run.sh exec jsq /tmp/ev_100mb.json queries/q3.jsq
./run.sh exec jq  /tmp/ev_100mb.json queries/q3.jq
./run.sh exec jaq /tmp/ev_100mb.json queries/q3.jq
```

`JSQ=/path/to/jsq ./run.sh …` overrides which binary is used.
