#!/usr/bin/env python3
"""
Stream-generate a large nested-JSON file for stress-testing BigJSON.

Top level:
{
  "meta": { ... },
  "users":    [ ...big array of user objects... ],
  "events":   [ ...big array of event objects... ],
  "products": [ ...big array of product objects... ]
}

Each record is itself a multi-level nested object with mixed-type
fields, optional keys, nested arrays, and nulls — so the navigation
view, type histograms, and queries get exercised against realistic
heterogeneity.

Usage:
    scripts/gen_stress_json.py [target_path] [target_gb]
"""
from __future__ import annotations

import json
import os
import random
import string
import sys
import time

TARGET_PATH = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/bigjson-stress.json")
TARGET_GB   = float(sys.argv[2]) if len(sys.argv) > 2 else 10.0
TARGET_BYTES = int(TARGET_GB * 1024 * 1024 * 1024)

random.seed(0xB16_5500)

# ---- vocab ----------------------------------------------------------------

CITIES = ["Tokyo", "Paris", "Berlin", "Cairo", "Lagos", "Lima", "Dakar",
          "Athens", "Oslo", "Sofia", "Manila", "Hanoi", "Quito", "Kyiv",
          "Riga", "Doha", "Muscat", "Bogotá", "Caracas", "Auckland",
          "Reykjavík", "Marrakech", "Almaty", "Tashkent", "Seoul", "Osaka"]

PRODUCT_NAMES = ["Cog", "Widget", "Sprocket", "Gizmo", "Doodad", "Thingamajig",
                 "Contraption", "Apparatus", "Mechanism", "Module", "Unit",
                 "Assembly", "Component", "Element"]

ADJECTIVES = ["quantum", "graphene", "smart", "eco", "nano", "industrial",
              "premium", "compact", "modular", "hybrid", "kinetic", "thermal"]

EVENT_TYPES = ["click", "view", "purchase", "signup", "logout", "login",
               "error", "warning", "share", "comment", "follow", "unfollow",
               "search", "filter", "scroll", "hover", "publish", "delete"]

LANGUAGES = ["en", "es", "fr", "de", "ja", "zh", "ar", "ru", "pt", "hi", "ko"]
THEMES    = ["dark", "light", "auto"]
ROLES     = ["user", "admin", "viewer", "editor", "auditor", "owner"]
TAGS_POOL = ["new", "vip", "beta", "trial", "expired", "premium", "active",
             "inactive", "flagged", "verified", "spam", "lead", "loyal"]


def rand_word(min_len=4, max_len=10):
    n = random.randint(min_len, max_len)
    return "".join(random.choices(string.ascii_lowercase, k=n))


def rand_phrase(words=3):
    return " ".join(rand_word(3, 9) for _ in range(words))


def rand_email():
    return f"{rand_word(5, 9)}.{rand_word(4, 8)}@{rand_word(4, 8)}.{random.choice(['com','net','io','dev','org'])}"


def rand_iso(year_lo=2024, year_hi=2026):
    return (f"{random.randint(year_lo, year_hi):04d}-"
            f"{random.randint(1, 12):02d}-"
            f"{random.randint(1, 28):02d}T"
            f"{random.randint(0, 23):02d}:"
            f"{random.randint(0, 59):02d}:"
            f"{random.randint(0, 59):02d}Z")


# ---- record factories -----------------------------------------------------

def gen_user(uid: int):
    obj = {
        "id": uid,
        "username": rand_word(6, 14),
        "email": rand_email(),
        "age": random.randint(18, 85),
        "verified": random.random() > 0.4,
        "balance": round(random.uniform(-1000, 25000), 2),
        "role": random.choice(ROLES),
        "address": {
            "street": f"{random.randint(1, 9999)} {rand_word(4, 9).title()} St",
            "city": random.choice(CITIES),
            "zip": f"{random.randint(10000, 99999)}",
            "coords": {
                "lat": round(random.uniform(-90, 90), 5),
                "lng": round(random.uniform(-180, 180), 5),
            },
        },
        "tags": random.sample(TAGS_POOL, k=random.randint(0, 4)),
        "scores": [random.randint(0, 100) for _ in range(random.randint(0, 6))],
        "metadata": {
            "created_at": rand_iso(),
            "last_login": None if random.random() < 0.1 else rand_iso(),
            "preferences": {
                "theme": random.choice(THEMES),
                "notifications": random.random() > 0.3,
                "language": random.choice(LANGUAGES),
                "timezone_offset": random.choice([-12, -8, -5, 0, 1, 3, 5, 8, 9]),
            },
            "history": [
                {
                    "action": random.choice(EVENT_TYPES),
                    "at": rand_iso(),
                    "ok": random.random() > 0.05,
                }
                for _ in range(random.randint(0, 4))
            ],
        },
    }
    if random.random() < 0.3:
        obj["nickname"] = rand_word(4, 10)
    if random.random() < 0.2:
        obj["bio"] = rand_phrase(random.randint(4, 16))
    return obj


def gen_event(eid: int):
    return {
        "id": eid,
        "type": random.choice(EVENT_TYPES),
        "ts": rand_iso(),
        "user_id": random.randint(0, 1_000_000),
        "session": rand_word(16, 24),
        "ok": random.random() > 0.05,
        "latency_ms": round(random.expovariate(1 / 50), 2),
        "context": {
            "page": "/" + "/".join(rand_word(3, 8) for _ in range(random.randint(1, 3))),
            "ip": ".".join(str(random.randint(0, 255)) for _ in range(4)),
            "ua": random.choice(["chrome", "firefox", "safari", "edge"]) + f"/{random.randint(80, 130)}",
            "geo": {
                "city": random.choice(CITIES),
                "country": rand_word(2, 2).upper(),
            } if random.random() > 0.2 else None,
        },
        "payload": {
            "values": [round(random.uniform(0, 100), 3) for _ in range(random.randint(0, 5))],
            "labels": [rand_word(3, 8) for _ in range(random.randint(0, 4))],
            "extra": None,
        } if random.random() > 0.3 else {},
    }


def gen_product(pid: int):
    return {
        "id": pid,
        "sku": f"{rand_word(3,3).upper()}-{random.randint(1000, 99999)}",
        "name": f"{random.choice(ADJECTIVES).title()} {random.choice(PRODUCT_NAMES)}",
        "price": round(random.uniform(1, 9999), 2),
        "in_stock": random.randint(0, 5000),
        "rating": round(random.uniform(0, 5), 2),
        "categories": random.sample(TAGS_POOL, k=random.randint(1, 3)),
        "specs": {
            "weight_g": random.randint(10, 25_000),
            "dims_mm": {
                "w": random.randint(1, 800),
                "h": random.randint(1, 800),
                "d": random.randint(1, 800),
            },
            "color_options": [rand_word(4, 8) for _ in range(random.randint(0, 5))],
        },
        "reviews_summary": {
            "count": random.randint(0, 5000),
            "avg": round(random.uniform(1, 5), 2),
            "buckets": {str(k): random.randint(0, 1000) for k in range(1, 6)},
        },
        "discontinued": random.random() < 0.05,
    }


# ---- writer ---------------------------------------------------------------

# Dump compactly to maximise content density (more nodes per GB).
DUMPS = json.JSONEncoder(separators=(",", ":")).encode


def main():
    print(f"Target: {TARGET_GB:.2f} GB → {TARGET_PATH}")
    started = time.time()
    bytes_written = 0
    uid = 0
    eid = 0
    pid = 0

    # Distribute the budget roughly: 50% users, 35% events, 15% products.
    user_budget    = int(TARGET_BYTES * 0.50)
    event_budget   = int(TARGET_BYTES * 0.35)
    # products gets the remainder

    with open(TARGET_PATH, "wb", buffering=8 * 1024 * 1024) as f:
        def w(s: str):
            nonlocal bytes_written
            b = s.encode("utf-8")
            f.write(b)
            bytes_written += len(b)

        w('{\n')
        w(f'"meta":{{"generated_at":"{rand_iso()}","target_gb":{TARGET_GB},"seed":"0xB16_5500"}},\n')

        # users
        w('"users":[')
        first = True
        progress = bytes_written
        while bytes_written < progress + user_budget:
            if not first: w(',')
            first = False
            w(DUMPS(gen_user(uid)))
            uid += 1
            if uid & 0xFFFF == 0:
                gb = bytes_written / (1024**3)
                rate = bytes_written / max(time.time() - started, 1e-3) / (1024**2)
                print(f"\r users: {uid:>9}  {gb:6.3f} GB  {rate:6.1f} MB/s", end="", flush=True)
        w(']')
        print()

        # events
        w(',\n"events":[')
        first = True
        progress = bytes_written
        while bytes_written < progress + event_budget:
            if not first: w(',')
            first = False
            w(DUMPS(gen_event(eid)))
            eid += 1
            if eid & 0xFFFF == 0:
                gb = bytes_written / (1024**3)
                rate = bytes_written / max(time.time() - started, 1e-3) / (1024**2)
                print(f"\r events:{eid:>9}  {gb:6.3f} GB  {rate:6.1f} MB/s", end="", flush=True)
        w(']')
        print()

        # products
        w(',\n"products":[')
        first = True
        while bytes_written < TARGET_BYTES:
            if not first: w(',')
            first = False
            w(DUMPS(gen_product(pid)))
            pid += 1
            if pid & 0xFFFF == 0:
                gb = bytes_written / (1024**3)
                rate = bytes_written / max(time.time() - started, 1e-3) / (1024**2)
                print(f"\r prods: {pid:>9}  {gb:6.3f} GB  {rate:6.1f} MB/s", end="", flush=True)
        w(']\n}\n')
        print()

    elapsed = time.time() - started
    final_gb = bytes_written / (1024**3)
    print(f"Done: {final_gb:.3f} GB in {elapsed:.1f}s "
          f"({uid} users, {eid} events, {pid} products) → {TARGET_PATH}")


if __name__ == "__main__":
    main()
