#!/usr/bin/env python3
"""Generate a deterministic JSON file of an approximate size for benchmarking.

Usage:
    python3 gen.py --target-mb 100 --out events_100mb.json
    python3 gen.py --target-bytes 1000000000 --out events_1gb.json

The output is a single JSON object with three arrays:

    {"events": [ ... ],     # the fact table — drives the file size
     "users":  [ ... ],     # a 10k-row dimension, foreign-keyed by user_id
     "regions":[ ... ]}     # a 5-row dimension, keyed by region code

`events` is the large array a streaming engine and a whole-file parser both
have to deal with; `users` and `regions` exist so the join benchmarks have
something to join against. The dimensions are generated from their own seeded
RNG, so they are byte-identical across every file size. Everything is seeded,
so the same target size always produces the same file.
"""
import argparse
import json
import random

REGIONS = ["EU", "US", "APAC", "LATAM", "MEA"]
STATUSES = ["paid", "open", "refunded", "cancelled"]
SKUS = ["A100", "B200", "C300", "D400", "E500"]
TIERS = ["free", "pro", "enterprise"]
COUNTRY = {"EU": "Germany", "US": "USA", "APAC": "Japan", "LATAM": "Brazil", "MEA": "UAE"}

N_USERS = 10_000


def event(i: int, rng: random.Random) -> dict:
    n_items = rng.randint(1, 3)
    return {
        "id": i,
        "user_id": rng.randint(1, N_USERS),
        "region": rng.choice(REGIONS),
        "status": rng.choice(STATUSES),
        "amount": round(rng.uniform(1.0, 1000.0), 2),
        "ts": 1_700_000_000 + rng.randint(0, 30_000_000),
        "items": [
            {"sku": rng.choice(SKUS), "qty": rng.randint(1, 5)}
            for _ in range(n_items)
        ],
    }


def users_json(seed: int) -> str:
    rng = random.Random(seed + 1)
    rows = [
        {
            "user_id": uid,
            "name": f"user_{uid}",
            "tier": rng.choice(TIERS),
            "region": rng.choice(REGIONS),
        }
        for uid in range(1, N_USERS + 1)
    ]
    return ",".join(json.dumps(r, separators=(",", ":")) for r in rows)


def regions_json() -> str:
    rows = [
        {"code": c, "country": COUNTRY[c], "manager": f"mgr_{c}"}
        for c in REGIONS
    ]
    return ",".join(json.dumps(r, separators=(",", ":")) for r in rows)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--target-mb", type=float, default=None)
    ap.add_argument("--target-bytes", type=int, default=None)
    ap.add_argument("--out", required=True)
    ap.add_argument("--seed", type=int, default=42)
    args = ap.parse_args()

    if args.target_bytes is not None:
        target = args.target_bytes
    elif args.target_mb is not None:
        target = int(args.target_mb * 1024 * 1024)
    else:
        ap.error("pass --target-mb or --target-bytes")

    # The dimension tables are fixed-size; size the events array to fill the
    # remaining budget so the total file lands near the target.
    users = users_json(args.seed)
    regions = regions_json()
    tail = f'],"users":[{users}],"regions":[{regions}]}}'
    events_budget = target - len(tail) - len('{"events":[')

    rng = random.Random(args.seed)
    written = 0
    i = 0
    with open(args.out, "w") as f:
        written += f.write('{"events":[')
        first = True
        # Write events in batches to amortize I/O until the events budget fills.
        while written < events_budget:
            parts = [json.dumps(event(i + k, rng), separators=(",", ":")) for k in range(1000)]
            i += 1000
            text = ("" if first else ",") + ",".join(parts)
            written += f.write(text)
            first = False
        written += f.write(tail)
    print(
        f"wrote {args.out}: {written} bytes ({written / 1024 / 1024:.1f} MiB), "
        f"{i} events, {N_USERS} users, {len(REGIONS)} regions"
    )


if __name__ == "__main__":
    main()
