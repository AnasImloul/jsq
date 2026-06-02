#!/usr/bin/env python3
"""Generate a deterministic {"events": [...]} JSON file of an approximate size.

Usage:
    python3 gen.py --target-mb 100 --out events_100mb.json
    python3 gen.py --target-bytes 1000000000 --out events_1gb.json

The output is a single JSON object with one large array, which is the shape a
streaming query engine and a whole-file parser both have to deal with. The data
is seeded, so the same target size always produces the same file.
"""
import argparse
import json
import random

REGIONS = ["EU", "US", "APAC", "LATAM", "MEA"]
STATUSES = ["paid", "open", "refunded", "cancelled"]
SKUS = ["A100", "B200", "C300", "D400", "E500"]


def event(i: int, rng: random.Random) -> dict:
    n_items = rng.randint(1, 3)
    return {
        "id": i,
        "user_id": rng.randint(1, 100_000),
        "region": rng.choice(REGIONS),
        "status": rng.choice(STATUSES),
        "amount": round(rng.uniform(1.0, 1000.0), 2),
        "ts": 1_700_000_000 + rng.randint(0, 30_000_000),
        "items": [
            {"sku": rng.choice(SKUS), "qty": rng.randint(1, 5)}
            for _ in range(n_items)
        ],
    }


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

    rng = random.Random(args.seed)
    written = 0
    i = 0
    with open(args.out, "w") as f:
        written += f.write('{"events":[')
        first = True
        # Leave headroom for the closing "]}". Write in batches to amortize I/O.
        while written < target - 16:
            parts = [json.dumps(event(i + k, rng), separators=(",", ":")) for k in range(1000)]
            i += 1000
            text = ("" if first else ",") + ",".join(parts)
            written += f.write(text)
            first = False
        written += f.write("]}")
    print(f"wrote {args.out}: {written} bytes ({written / 1024 / 1024:.1f} MiB), {i} events")


if __name__ == "__main__":
    main()
