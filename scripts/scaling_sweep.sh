#!/bin/bash
# Scaling sweep for the group capping policy (ADR 0033) and its regression suite (ADR 0034).
#
# Two task shapes, because the policy turns on the difference between them:
#   A  one crate per task      - one thread each
#   B  six crates per task     - six threads each, built with xargs -P6
#
# Every run is pinned with taskset, so a "two-core" row is a two-core machine as far as the compiler
# can tell, and every task gets a cold target directory so nothing is warmed by what ran before it.
# No modelling: what goes in the file is what the machine did.
#
# "Two-core" means two cores, not two hardware threads. The pin takes one logical CPU per physical
# core, so a width only ever counts real cores: two threads of one core do not deliver two cores'
# worth of parallelism, and a row measured on them is not comparable with a row measured on two
# cores. The meta record says how many cores and how many threads the machine had, and each row
# says which CPUs it was pinned to.
#
# Output is JSON lines: one `meta` record, then one record per cell with the median over repeats and
# the spread between them.
#
# Usage:
#   scripts/scaling_sweep.sh OUT.json
# Environment:
#   CORES="1 2 3 4 6"   core counts to pin (clamped to nproc)
#   CONC="1 2 3 4 6 8"  concurrency levels to try
#   SHAPES="A B"        task shapes to measure
#   REPEATS=2           runs per cell; the spread is what the regression suite calls noise
#   REF_REPEATS=4       runs for the sequential (concurrency 1) cell, which is every row's denominator
#   FUNCS=1500          functions per fixture crate, i.e. how much codegen one crate costs
set -u
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR=never

OUT=${1:-/tmp/scaling.json}
ROOT=/tmp/scalefix
CRATES_BUNDLES=3
PER_BUNDLE=6
FUNCS=${FUNCS:-1500}
REPEATS=${REPEATS:-2}
REF_REPEATS=${REF_REPEATS:-4}
CORES=${CORES:-"1 2 3 4 6"}
CONC=${CONC:-"1 2 3 4 6 8"}
SHAPES=${SHAPES:-"A B"}
CRATES_A=8
CRATES_TOTAL=$(( CRATES_BUNDLES * PER_BUNDLE > CRATES_A ? CRATES_BUNDLES * PER_BUNDLE : CRATES_A ))
NPROC=$(nproc)
TMP=$(mktemp)

# Physical cores, not hardware threads. `cpu_topology.py` reads the machine's own topology and picks
# one CPU per core; where sysfs does not say (a kernel without topology, a hypervisor that hides it),
# it falls back to one logical CPU per core and says so, and the meta record carries that answer.
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
read -r PHYSICAL_CORES PIN_CPUS PIN_SOURCE < <(python3 "$SCRIPT_DIR/cpu_topology.py")
if [ -z "${PIN_SOURCE:-}" ]; then
  echo "could not read the machine's CPU topology" >&2
  exit 1
fi

# The first `$1` pinned CPUs, as taskset wants them: a comma-separated list.
pin_for() {
  local want=$1 seen=0 out="" cpu
  local -a cpus
  IFS=',' read -r -a cpus <<< "$PIN_CPUS"
  for cpu in "${cpus[@]}"; do
    [ "$seen" -ge "$want" ] && break
    if [ -z "$out" ]; then out=$cpu; else out="$out,$cpu"; fi
    seen=$((seen + 1))
  done
  echo "$out"
}

now_ms() { date +%s%3N; }

build_fixture() {
  rm -rf "$ROOT"; mkdir -p "$ROOT"
  for i in $(seq 1 "$CRATES_TOTAL"); do
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

# Shape A: task = one crate.
run_a() {
  local cores=$1 conc=$2 dir="$3" s e running i
  rm -rf "$dir"; s=$(now_ms); running=0
  for i in $(seq 1 $CRATES_A); do
    if [ "$running" -ge "$conc" ]; then wait -n; running=$((running - 1)); fi
    ( cd "$ROOT/m$i" && CARGO_TARGET_DIR="$dir/$i" cargo build -q >/dev/null 2>&1 ) &
    running=$((running + 1))
  done
  wait
  e=$(now_ms); echo -e "A\t$cores\t$conc\t$CRATES_A\t1\t$((e - s))\t$PINS" >> "$TMP"
  rm -rf "$dir"
}

# Shape B: task = one bundle of $PER_BUNDLE crates, all of which really exist and really build.
run_b() {
  local cores=$1 conc=$2 dir="$3" s e running b first
  rm -rf "$dir"; s=$(now_ms); running=0
  for b in $(seq 1 $CRATES_BUNDLES); do
    first=$(( (b - 1) * PER_BUNDLE + 1 ))
    if [ "$running" -ge "$conc" ]; then wait -n; running=$((running - 1)); fi
    ( for k in $(seq "$first" $((first + PER_BUNDLE - 1))); do
        ( cd "$ROOT/m$k" && CARGO_TARGET_DIR="$dir/$k" cargo build -q >/dev/null 2>&1 ) &
      done
      wait ) &
    running=$((running + 1))
  done
  wait
  e=$(now_ms); echo -e "B\t$cores\t$conc\t$CRATES_BUNDLES\t$PER_BUNDLE\t$((e - s))\t$PINS" >> "$TMP"
  rm -rf "$dir"
}

build_fixture
: > "$TMP"
echo "fixture: $CRATES_TOTAL crates x $FUNCS functions; A = 1 crate/task, B = $PER_BUNDLE crates/task" >&2
echo "machine: $PHYSICAL_CORES cores over $NPROC threads, $(date -Iseconds), repeats $REPEATS, ref repeats $REF_REPEATS" >&2
echo "pinned CPUs per core width: $PIN_CPUS" >&2

for shape in $SHAPES; do
  for cores in $CORES; do
    if [ "$cores" -gt "$PHYSICAL_CORES" ]; then
      # A width wider than the machine's real cores would measure SMT, not cores. Skip it and say so,
      # rather than record a row that means something else on every other host.
      echo "skip $shape width $cores: the machine has $PHYSICAL_CORES physical cores" >&2
      continue
    fi
    pin=$(pin_for "$cores")
    for conc in $CONC; do
      if [ "$shape" = "A" ] && [ "$conc" -gt "$CRATES_A" ]; then continue; fi
      if [ "$shape" = "B" ] && [ "$conc" -gt "$CRATES_BUNDLES" ]; then continue; fi
      reps=$REPEATS
      [ "$conc" -eq 1 ] && reps=$REF_REPEATS
      for rep in $(seq 1 "$reps"); do
        if [ "$shape" = "A" ]; then
          taskset -c "$pin" bash -c "$(declare -f run_a now_ms); ROOT=$ROOT CRATES_A=$CRATES_A TMP=$TMP PINS=$pin; run_a $cores $conc $ROOT/t_a_${cores}_${conc}_${rep}"
        else
          taskset -c "$pin" bash -c "$(declare -f run_b now_ms); ROOT=$ROOT CRATES_BUNDLES=$CRATES_BUNDLES PER_BUNDLE=$PER_BUNDLE TMP=$TMP PINS=$pin; run_b $cores $conc $ROOT/t_b_${cores}_${conc}_${rep}"
        fi
      done
      echo "$shape cores=$cores conc=$conc done" >&2
    done
  done
done

python3 - "$TMP" "$OUT" "$NPROC" "$PHYSICAL_CORES" "$PIN_CPUS" "$PIN_SOURCE" "$REPEATS" "$FUNCS" "$CRATES_A" "$CRATES_BUNDLES" "$PER_BUNDLE" "$CORES" "$CONC" "$SHAPES" "$REF_REPEATS" <<'PY'
import json, platform, statistics, sys, collections

(tmp, out, nproc, physical, pin_cpus, pin_source, repeats, funcs, crates_a, bundles, per_bundle,
 cores, conc, shapes, ref_repeats) = sys.argv[1:16]
cells = collections.defaultdict(list)
with open(tmp) as handle:
    for line in handle:
        if not line.strip():
            continue
        shape, core_count, concurrency, tasks, threads, wall_ms, cpus = line.rstrip("\n").split("\t")
        cells[(shape, int(core_count), int(concurrency), int(tasks), int(threads))].append(
            (int(wall_ms), cpus)
        )

records = []
for (shape, cores_n, concurrency, tasks, threads), samples in sorted(cells.items()):
    walls = [wall for wall, _ in samples]
    median = statistics.median(walls)
    spread = (max(walls) - min(walls)) / median * 100.0 if len(walls) > 1 and median else 0.0
    records.append({
        "kind": "measurement",
        "shape": shape,
        "cores": cores_n,
        "concurrency": concurrency,
        "tasks": tasks,
        "threads": threads,
        "wall_ms": median,
        "wall": round(median / 1000.0, 3),
        "repeats": len(walls),
        "spread_pct": round(spread, 1),
        "cpus": samples[0][1],
    })

meta = {
    "kind": "meta",
    "schema": 3,
    "recorded": __import__("datetime").datetime.now().astimezone().isoformat(timespec="seconds"),
    "nproc": int(nproc),
    "physical_cores": int(physical),
    "logical_cpus": int(nproc),
    "pinning": "physical",
    "topology": pin_source,
    "pin_cpus": pin_cpus,
    "cores_swept": cores,
    "concurrency_swept": conc,
    "shapes": shapes,
    "repeats": int(repeats),
    "ref_repeats": int(ref_repeats),
    "fixture": {
        "funcs_per_crate": int(funcs),
        "crates_one_per_task": int(crates_a),
        "bundles": int(bundles),
        "crates_per_bundle": int(per_bundle),
    },
    "machine": platform.platform(),
    "processor": platform.processor() or "unknown",
}

with open(out, "w") as handle:
    handle.write(json.dumps(meta) + "\n")
    for record in records:
        handle.write(json.dumps(record) + "\n")

print(f"wrote {out}: {len(records)} cells from {sum(r['repeats'] for r in records)} pinned runs", file=sys.stderr)
PY

rm -rf "$ROOT" "$TMP"
