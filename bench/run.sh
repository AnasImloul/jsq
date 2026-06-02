#!/usr/bin/env bash
# Benchmark jsq against jq on a generated events file.
#
#   bench/run.sh exec <jsq|jq> <FILE> <QFILE>   # run one query once (output -> stdout)
#   bench/run.sh bench <FILE> <LABEL> [RUNS]     # full sweep, prints a markdown table
#
# `bench` measures median wall time (hyperfine) and peak resident memory
# (/usr/bin/time -l, macOS) for each of the four queries, for both tools.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
JSQ="${JSQ:-$HERE/../engine/target/release/jsq}"
QDIR="$HERE/queries"

case "${1:-}" in
exec)
    tool="$2"; file="$3"; qfile="$4"
    q="$(cat "$qfile")"
    case "$tool" in
        jsq) exec "$JSQ" "$file" "$q" ;;
        jq)  exec jq "$q" "$file" ;;
        *) echo "unknown tool: $tool" >&2; exit 2 ;;
    esac
    ;;
bench)
    file="$2"; label="$3"; runs="${4:-5}"
    peak_mib() { # tool qfile -> peak RSS in MiB
        local t; t="$(mktemp)"
        /usr/bin/time -l "$HERE/run.sh" exec "$1" "$file" "$2" >/dev/null 2>"$t" || true
        awk '/maximum resident set size/ {printf "%.0f", $1/1048576}' "$t"
        rm -f "$t"
    }
    median_s() { # tool qfile -> median seconds
        local j; j="$(mktemp)"
        hyperfine --warmup 1 --runs "$runs" --export-json "$j" \
            "$HERE/run.sh exec $1 $file $2" >/dev/null 2>&1
        jq -r '.results[0].median' "$j"
        rm -f "$j"
    }
    echo "### $label"
    echo
    echo "| Query | jsq time | jq time | speedup | jsq peak RSS | jq peak RSS |"
    echo "|-------|---------:|--------:|--------:|-------------:|------------:|"
    for q in q1 q2 q3 q4; do
        jsqf="$QDIR/$q.jsq"; jqf="$QDIR/$q.jq"
        jt="$(median_s jsq "$jsqf")"; qt="$(median_s jq "$jqf")"
        jm="$(peak_mib jsq "$jsqf")"; qm="$(peak_mib jq "$jqf")"
        sp="$(awk -v a="$qt" -v b="$jt" 'BEGIN{printf "%.1fx", a/b}')"
        printf "| %s | %.3fs | %.3fs | %s | %s MiB | %s MiB |\n" \
            "$q" "$jt" "$qt" "$sp" "$jm" "$qm"
    done
    echo
    ;;
*)
    echo "usage: run.sh exec <jsq|jq> <FILE> <QFILE> | run.sh bench <FILE> <LABEL> [RUNS]" >&2
    exit 2
    ;;
esac
