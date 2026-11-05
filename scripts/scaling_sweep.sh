#!/bin/bash
# Scaling curves for the group capping policy (ADR 0033): wall clock against core count and threads
# per task, measured rather than modelled.
#
# Two shapes, because the policy turns on the difference between them:
#   A  one crate's codegen per task      - one thread each
#   B  a six-crate bundle per task        - six threads each, built with xargs -P6
#
# Cores are real: every run is pinned with taskset, so a "two-core" row is a two-core machine as far
# as the compiler can tell. Every configuration gets fresh target directories, so nothing is warmed by
# the configuration before it.
#
# Usage: scripts/scaling_sweep.sh <output.json>
set -u
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR=never
OUT=${1:-/tmp/scaling.json}
ROOT=/tmp/scalefix
CRATES=8          # shape A tasks
BUNDLES=3         # shape B tasks
PER_BUNDLE=6      # crates inside one shape B task, i.e. threads per task
FUNCS=3000

build_fixture() {
  rm -rf "$ROOT"; mkdir -p "$ROOT"
  for i in $(seq 1 $CRATES); do
    mkdir -p "$ROOT/m$i/src"
    cat > "$ROOT/m$i/Cargo.toml" <<TOML
[package]
name = "m$i"
version = "0.1.0"
edition = "2021"
TOML
    python3 - "$i" "$FUNCS" > "$ROOT/m$i/src/lib.rs" <<'PY'
import sys
seed, funcs = int(sys.argv[1]), int(sys.argv[2])
out = ["//! Scaling fixture.", ""]
for f in range(funcs):
    n = f * 7 + seed
    width = 2 + n % 6
    out += [
        f"pub fn f{n}(x: u64) -> [u64; {width}] {{",
        f"    let mut a = [x; {width}];",
        f"    for i in 0..a.len() {{ a[i] = a[i].wrapping_mul({n % 97 + 1}).wrapping_add(i as u64); }}",
        "    a",
        "}",
    ]
print("\n".join(out))
PY
  done
}

now() { date +%s; }

record() { printf '{"shape": "%s", "cores": %s, "concurrency": %s, "tasks": %s, "threads": %s, "wall": %s}\n' \
  "$1" "$2" "$3" "$4" "$5" "$6" >> "$OUT"; }

# Shape A: task = one crate. `conc` tasks at a time, each pinned by the shell's own affinity.
run_a() {
  local cores=$1 conc=$2 dir="$ROOT/t_a_${cores}_${conc}" s e running i
  rm -rf "$dir"; s=$(now)
  running=0
  for i in $(seq 1 $CRATES); do
    if [ "$running" -ge "$conc" ]; then wait -n; running=$((running - 1)); fi
    ( cd "$ROOT/m$i" && CARGO_TARGET_DIR="$dir/$i" cargo build -q >/dev/null 2>&1 ) &
    running=$((running + 1))
  done
  wait
  e=$(now); record A "$cores" "$conc" "$CRATES" 1 $((e - s))
  rm -rf "$dir"
}

# Shape B: task = one bundle of six crates, six builds in flight inside the task.
run_b() {
  local cores=$1 conc=$2 s e running b first
  rm -rf "$ROOT/t_b"; s=$(now)
  running=0
  for b in $(seq 1 $BUNDLES); do
    first=$(( (b - 1) * PER_BUNDLE + 1 ))
    if [ "$running" -ge "$conc" ]; then wait -n; running=$((running - 1)); fi
    ( for k in $(seq "$first" $((first + PER_BUNDLE - 1))); do
        ( cd "$ROOT/m$k" && CARGO_TARGET_DIR="$ROOT/t_b/$k" cargo build -q >/dev/null 2>&1 ) &
      done
      wait ) &
    running=$((running + 1))
  done
  wait
  e=$(now); record B "$cores" "$conc" "$BUNDLES" "$PER_BUNDLE" $((e - s))
  rm -rf "$ROOT/t_b"
}

build_fixture
: > "$OUT"
echo "fixture: $CRATES crates x $FUNCS functions; shape A = 1 crate/task, shape B = $PER_BUNDLE crates/task" >&2
echo "machine: $(nproc) cores, $(date -Iseconds)" >&2

for cores in 1 2 3 6; do
  for conc in 1 2 3 6 8; do
    [ "$conc" -gt "$CRATES" ] && continue
    taskset -c "0-$((cores - 1))" bash -c "$(declare -f run_a record now); ROOT=$ROOT CRATES=$CRATES OUT=$OUT; run_a $cores $conc"
    echo "A cores=$cores conc=$conc -> $(tail -1 "$OUT")" >&2
  done
done

for cores in 1 3 6; do
  for conc in 1 2 3; do
    taskset -c "0-$((cores - 1))" bash -c "$(declare -f run_b record now); ROOT=$ROOT BUNDLES=$BUNDLES PER_BUNDLE=$PER_BUNDLE OUT=$OUT; run_b $cores $conc"
    echo "B cores=$cores conc=$conc -> $(tail -1 "$OUT")" >&2
  done
done

rm -rf "$ROOT"
echo "wrote $OUT ($(wc -l < "$OUT") rows)" >&2
