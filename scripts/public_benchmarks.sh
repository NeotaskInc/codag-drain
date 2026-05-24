#!/usr/bin/env bash
set -euo pipefail

# Public deterministic parser benchmark surface for codag-drain.
#
# This runs the LogHub-2.0 grouping/compression evals across the public systems
# available in LOGHUB_DIR. It makes no model calls.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

: "${LOGHUB_DIR:?Set LOGHUB_DIR to the LogHub-2.0 structured CSV root}"
export LOGHUB_SYSTEMS="${LOGHUB_SYSTEMS:-Apache,BGL,HDFS,HPC,Hadoop,HealthApp,Linux,Mac,OpenSSH,OpenStack,Proxifier,Spark,Thunderbird,Zookeeper}"
export LOGHUB_LIMIT="${LOGHUB_LIMIT:-3000}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/private/tmp/codag-drain-target}"

cd "$ROOT"

echo "== codag-drain public LogHub grouping benchmark =="
cargo test -p codag-drain --test eval_loghub grouping_loghub -- --ignored --nocapture

echo
echo "== codag-drain public LogHub compression benchmark =="
cargo test -p codag-drain --test eval_loghub compression_loghub -- --ignored --nocapture

echo
echo "== codag-drain public LogHub timing benchmark =="
cargo test -p codag-drain --test eval_loghub timing_loghub_default_vs_drain3 -- --ignored --nocapture
