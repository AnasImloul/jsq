#!/usr/bin/env bash
# Benchmark jsq against jq and jaq on a generated events file.
#
#   bench/run.sh exec <jsq|jq|jaq> <FILE> <QFILE>   # run one query once (output -> stdout)
#   bench/run.sh bench <FILE> <LABEL> [RUNS]         # full sweep, prints markdown tables
#
# `bench` measures median wall time (hyperfine) and peak memory
# (/usr/bin/time -l, macOS) for each of the four queries, for all three
# tools. jq and jaq run the same .jq filter (jaq is a jq reimplementation
# in Rust), so the pair isolates streaming-vs-full-parse from C-vs-Rust.
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
        jq)  exec jq  "$q" "$file" ;;
        jaq) exec jaq "$q" "$file" ;;
        *) echo "unknown tool: $tool" >&2; exit 2 ;;
    esac
    ;;
bench)
    file="$2"; label="$3"; runs="${4:-5}"
    # Returns "<footprint_mib> <rss_mib>". "peak memory footprint" is the
    # dirty+compressed memory Activity Monitor shows as "Memory"; RSS also
    # counts clean, reclaimable mmap'd pages.
    peak_mem() { # tool qfile -> "<footprint_mib> <rss_mib>"
        local t; t="$(mktemp)"
        /usr/bin/time -l "$HERE/run.sh" exec "$1" "$file" "$2" >/dev/null 2>"$t" || true
        awk '
            /maximum resident set size/ {rss=$1}
            /peak memory footprint/   {fp=$1}
            END {printf "%.0f %.0f", fp/1048576, rss/1048576}
        ' "$t"
        rm -f "$t"
    }
    median_s() { # tool qfile -> median seconds
        local j; j="$(mktemp)"
        hyperfine --warmup 1 --runs "$runs" --export-json "$j" \
            "$HERE/run.sh exec $1 $file $2" >/dev/null 2>&1
        jq -r '.results[0].median' "$j"
        rm -f "$j"
    }
    # jsq has its own query file; jq and jaq share the .jq filter.
    qf() { # tool q -> query file path
        case "$1" in jsq) echo "$QDIR/$2.jsq" ;; *) echo "$QDIR/$2.jq" ;; esac
    }

    echo "### $label"
    echo
    echo "**Median wall time (s)**"
    echo
    echo "| Query | jsq | jq | jaq |"
    echo "|-------|----:|---:|----:|"
    for q in q1 q2 q3 q4 q5 q6 q7 q8 q9; do
        jt="$(median_s jsq "$(qf jsq $q)")"
        qt="$(median_s jq  "$(qf jq  $q)")"
        at="$(median_s jaq "$(qf jaq $q)")"
        printf "| %s | %.3f | %.3f | %.3f |\n" "$q" "$jt" "$qt" "$at"
    done
    echo
    echo "**Peak memory — RAM (footprint) / RSS, MiB**"
    echo
    echo "| Query | jsq RAM | jq RAM | jaq RAM | jsq RSS | jq RSS | jaq RSS |"
    echo "|-------|--------:|-------:|--------:|--------:|-------:|--------:|"
    for q in q1 q2 q3 q4 q5 q6 q7 q8 q9; do
        read -r jfp jrss <<<"$(peak_mem jsq "$(qf jsq $q)")"
        read -r qfp qrss <<<"$(peak_mem jq  "$(qf jq  $q)")"
        read -r afp arss <<<"$(peak_mem jaq "$(qf jaq $q)")"
        printf "| %s | %s | %s | %s | %s | %s | %s |\n" \
            "$q" "$jfp" "$qfp" "$afp" "$jrss" "$qrss" "$arss"
    done
    echo
    ;;
*)
    echo "usage: run.sh exec <jsq|jq|jaq> <FILE> <QFILE> | run.sh bench <FILE> <LABEL> [RUNS]" >&2
    exit 2
    ;;
esac
