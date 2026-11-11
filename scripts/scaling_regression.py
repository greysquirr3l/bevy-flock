#!/usr/bin/env python3
"""Fail CI when group scaling gets worse than the recorded baseline in `docs/dev/tuning/data/scaling-sweep.json`.

Wall clock alone cannot gate CI: a runner with four slower cores takes longer than the six-core
machine the baseline came from, and nothing has regressed. What transfers is the *shape* of the
curve — how much of the machine a given concurrency actually uses. So the comparison is on speedup
against the same core count's sequential run, which is dimensionless:

    speedup(cores, concurrency) = wall(cores, 1) / wall(cores, concurrency)

A drop in that ratio means the machine used its cores less well than it used to, on any hardware.

Two runs must have measured the same work to be comparable, so the fixture is checked first: a
different functions-per-crate or a different bundle size makes the comparison meaningless and is
refused rather than reported as a regression.

Two runs must also have measured the same *hardware* to be comparable, so core counts are read as
counts of real cores. A sweep pins one logical CPU per physical core and records the machine's
topology, so its widths mean the same thing on a four-core workstation and a four-core CI runner. The
recorded baseline was made before that: it pinned logical CPU ids on a host whose SMT layout was never
captured, so its wider rows count hardware threads, and a row of threads is not a row of cores. Those
widths are named and left out of the comparison rather than compared and failed.

    python3 scripts/scaling_regression.py docs/dev/tuning/data/scaling-sweep.json /tmp/candidate.json
    python3 scripts/scaling_regression.py BASE.json CAND.json --fail-pct 30 --warn-pct 15 --json out.json

Defaults are 25% to fail and 12% to warn. Both widen to twice the measured spread, so a noisy runner
gets slack without anyone editing the numbers. The spread is the sweep's, and the sweep takes it over
the samples that are not the slowest: a run a noisy neighbour stole from moves neither the median nor
the noise, and reading it as noise would blind the gate for a reason that has nothing to do with the
code.

A cell whose own spread is so wide that the failure threshold rises past the SLA it is supposed to
enforce is reported as `unverified`, never `ok`: it has not been checked, and saying otherwise would be
a green tick over an empty test. The sequential cell's spread is folded into every row that divides by
it, because a noisy denominator moves five rows at once — in the direction of looking faster.

Exit codes: 0 every comparable cell held, 1 a cell dropped beyond the failure threshold, 2 the two
files cannot be compared at all (different fixture, no overlapping core counts, or not one cell that
could be verified).
"""

from __future__ import annotations

import argparse
import json
import pathlib
import statistics
import sys

# A single run of a two-second compile carries a few percent of scheduler and filesystem noise. Below
# this, a difference is not worth a human's attention on any machine.
NOISE_FLOOR_PCT = 8.0
# The tolerances ADR 0034 commits to. Kept here so the report quotes the gate's numbers instead of
# restating them in prose, where they drifted once already.
DEFAULT_FAIL_PCT = 25.0
DEFAULT_WARN_PCT = 12.0
# How far the measured spread is allowed to widen each threshold: a warning that fires inside the
# noise is not a warning, and a failure needs twice the evidence a warning does.
WARN_NOISE_MULTIPLIER = 1.5
FAIL_NOISE_MULTIPLIER = 2.0
# A suite where most cells could not be checked is not a passing suite. Half is a blunt line, but it is
# a line: below it, the run says "cannot verify" instead of reporting a green tick on a coin toss.
MIN_VERIFIED_FRACTION = 0.5
# The recorded baseline was made before the sweep recorded the machine's topology. It pinned logical CPU
# ids (`taskset -c 0-N-1`) on a host whose SMT layout was never captured, so a row's core count there is
# a count of hardware threads, and a thread of a core is not a core: two threads of one core deliver
# about 1.3x, not the 2x a two-core row needs to be comparable with this machine's two-core row. Rows 1
# and 2 of that baseline are real cores: the width-2 row reaches 1.878x on two concurrent builds, which
# one core's two threads cannot do. Wider rows are ambiguous. Width 4 measures 3.734x, which four cores
# give at 93% efficiency and three cores plus one sibling cannot beat, but a host that pairs its threads
# differently could still have delivered it from three. So the gate compares the widths it can compare,
# names the rest, and a sweep recorded from schema 3 on has no such gap: every width in it is a count
# of real cores.
LEGACY_REAL_CORE_WIDTHS = frozenset({1, 2})


def load(path: str) -> tuple[dict, dict[tuple[str, int, int], dict]]:
    """Meta record and cells keyed by (shape, cores, concurrency); accepts schema 1 and 2."""
    meta: dict = {}
    cells: dict[tuple[str, int, int], dict] = {}
    for line in pathlib.Path(path).read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        record = json.loads(line)
        if record.get("kind") == "meta":
            meta = record
            continue
        wall = record.get("wall_ms", 0) / 1000.0 if "wall_ms" in record else float(record["wall"])
        cells[(record["shape"], int(record["cores"]), int(record["concurrency"]))] = {
            "wall": wall,
            "spread": float(record.get("spread_pct", 0.0) or 0.0),
            "tasks": int(record["tasks"]),
            "threads": int(record["threads"]),
        }
    return meta, cells


def speedup(cells: dict, shape: str, cores: int, concurrency: int) -> float | None:
    """Speedup against the same core count's sequential run, or None when either is missing."""
    sequential = cells.get((shape, cores, 1))
    cell = cells.get((shape, cores, concurrency))
    if not sequential or not cell or cell["wall"] <= 0:
        return None
    return sequential["wall"] / cell["wall"]


def real_core_widths(meta: dict, cells: dict) -> set[int]:
    """The core counts whose rows measured real cores rather than hardware threads.

    Schema 3 records the machine's physical cores and pins one CPU per core, so every width in the
    file counts real cores. Earlier sweeps pinned logical CPU ids without recording the host's SMT
    layout, so only the widths in `LEGACY_REAL_CORE_WIDTHS` are known to be real cores there.
    """
    widths = {key[1] for key in cells}
    if int(meta.get("schema", 1)) >= 3:
        return widths
    return widths & LEGACY_REAL_CORE_WIDTHS


def comparable_widths(
    base_meta: dict,
    base_cells: dict,
    cand_meta: dict,
    cand_cells: dict,
) -> set[int]:
    """Widths that mean the same thing on both sides: a count of real cores on each."""
    return real_core_widths(base_meta, base_cells) & real_core_widths(cand_meta, cand_cells)


def compare(
    base_cells: dict,
    cand_cells: dict,
    fail_pct: float = DEFAULT_FAIL_PCT,
    warn_pct: float = DEFAULT_WARN_PCT,
    widths: set[int] | None = None,
) -> tuple[list[dict], list[dict]]:
    """Per-cell verdicts and the list of cells that failed. Pure function so the self-test can drive it.

    `widths` restricts the comparison to core counts that mean the same thing on both sides; None
    compares every shared cell, which is what the self-test wants.
    """
    rows: list[dict] = []
    failures: list[dict] = []
    keys = sorted(set(base_cells) & set(cand_cells), key=lambda k: (k[0], k[1], k[2]))
    if widths is not None:
        keys = [key for key in keys if key[1] in widths]
    for key in keys:
        shape, cores, concurrency = key
        base = speedup(base_cells, shape, cores, concurrency)
        cand = speedup(cand_cells, shape, cores, concurrency)
        if base is None or cand is None or concurrency == 1:
            continue
        # The reference cell is the denominator of this row's speedup, so its noise is this row's noise:
        # a ragged sequential run inflates every speedup that divides by it, which reads as a gain.
        reference = (shape, cores, 1)
        noise = max(
            NOISE_FLOOR_PCT,
            base_cells[key]["spread"],
            cand_cells[key]["spread"],
            base_cells.get(reference, {}).get("spread", 0.0),
            cand_cells.get(reference, {}).get("spread", 0.0),
        )
        delta = (cand - base) / base * 100.0
        # A threshold tighter than the measurement's own spread is noise, not signal.
        fail_at = max(fail_pct, FAIL_NOISE_MULTIPLIER * noise)
        warn_at = max(warn_pct, WARN_NOISE_MULTIPLIER * noise)
        if delta <= -fail_at:
            status = "FAIL"
        elif delta <= -warn_at:
            status = "WARN"
        elif fail_at > fail_pct:
            # Noise has pushed the failure line past the tolerance this suite promises. The cell is not
            # green, it is untested, and the fix is more repeats rather than a wider threshold.
            status = "unverified"
        else:
            status = "ok"
        row = {
            "shape": shape,
            "cores": cores,
            "concurrency": concurrency,
            "baseline_speedup": round(base, 3),
            "candidate_speedup": round(cand, 3),
            "delta_pct": round(delta, 1),
            "noise_pct": round(noise, 1),
            "fail_at_pct": round(fail_at, 1),
            "warn_at_pct": round(warn_at, 1),
            "status": status,
        }
        rows.append(row)
        if status == "FAIL":
            failures.append(row)
    return rows, failures


def verdict_exit(rows: list[dict], failures: list[dict]) -> int:
    """0 pass, 1 a cell failed, 2 nothing (or too little) could be verified."""
    if failures:
        return 1
    verified = [r for r in rows if r["status"] != "unverified"]
    if not rows or len(verified) < MIN_VERIFIED_FRACTION * len(rows):
        return 2
    return 0


def coverage_notes(
    base_cells: dict,
    cand_cells: dict,
    base_meta: dict | None = None,
    cand_meta: dict | None = None,
) -> list[str]:
    """What the comparison did not cover, said out loud rather than left as a green tick."""
    base_cores = sorted({k[1] for k in base_cells})
    cand_cores = sorted({k[1] for k in cand_cells})
    notes = []
    skipped = [c for c in base_cores if c not in cand_cores]
    if skipped:
        notes.append(
            f"core counts in the baseline but not in the candidate (skipped): "
            f"{', '.join(map(str, skipped))}"
        )
    if not set(base_cores) & set(cand_cores):
        notes.append("no core count in common: the run cannot be compared with this baseline")
    if base_meta is not None and cand_meta is not None:
        base_real = real_core_widths(base_meta, base_cells)
        cand_real = real_core_widths(cand_meta, cand_cells)
        ambiguous = [c for c in base_cores if c not in base_real]
        if ambiguous:
            notes.append(
                "core counts the baseline recorded as hardware threads, not real cores, so they were "
                f"not compared: {', '.join(map(str, ambiguous))}"
            )
        unmatched = [c for c in cand_cores if c in cand_real and c not in base_real]
        if unmatched:
            notes.append(
                "core counts the candidate measured on real cores that the baseline has no real-core "
                f"row for, so they were not compared: {', '.join(map(str, unmatched))}"
            )
    return notes


def fixture_key(meta: dict) -> tuple:
    fixture = meta.get("fixture") or {}
    return (
        fixture.get("funcs_per_crate"),
        fixture.get("crates_one_per_task"),
        fixture.get("crates_per_bundle"),
    )


def self_test() -> int:
    failures: list[str] = []

    def cell(wall: float, spread: float = 0.0, threads: int = 1, tasks: int = 8) -> dict:
        return {"wall": wall, "spread": spread, "tasks": tasks, "threads": threads}

    def suite(walls: dict[int, float], shape: str = "A", cores: int = 4, spread: float = 0.0) -> dict:
        return {(shape, cores, conc): cell(wall, spread) for conc, wall in walls.items()}

    base = suite({1: 30.0, 2: 15.0, 4: 8.0})

    # 1. An identical run is not a regression.
    rows, dropped = compare(base, suite({1: 30.0, 2: 15.0, 4: 8.0}))
    if dropped or len(rows) != 2:
        failures.append(f"identical run reported {len(dropped)} failures over {len(rows)} cells")

    # 2. A cell whose speedup drops 36% must fail. Thresholds are on speedup, not wall clock: an
    #    8 s cell that becomes 12.5 s is a 36% speedup loss, because speedup is a ratio.
    rows, dropped = compare(base, suite({1: 30.0, 2: 15.0, 4: 12.5}))
    if len(dropped) != 1 or dropped[0]["concurrency"] != 4:
        failures.append(f"36% speedup regression produced {len(dropped)} failures")
    if dropped and abs(dropped[0]["delta_pct"] + 36.0) > 1.0:
        failures.append(f"regression delta reported as {dropped[0]['delta_pct']}, expected about -36%")

    # 3. A cell 20% slower is a warning, not a failure.
    rows, dropped = compare(base, suite({1: 30.0, 2: 15.0, 4: 9.6}))
    if dropped or not any(r["status"] == "WARN" for r in rows):
        failures.append("20% slowdown was not reported as a warning")

    # 4. Faster is fine.
    rows, dropped = compare(base, suite({1: 30.0, 2: 15.0, 4: 6.0}))
    if dropped or any(r["status"] != "ok" for r in rows):
        failures.append("a faster run was flagged")

    # 5. Noise raises the bar, both ways: with a 25% spread the fail line is 50% and the warn line
    #    37.5%, so a 29% speedup drop is neither. But a 50% failure line cannot enforce a 25% promise,
    #    so the cell is unverified rather than green.
    noisy = suite({1: 30.0, 2: 15.0, 4: 8.0}, spread=25.0)
    rows, dropped = compare(noisy, suite({1: 30.0, 2: 15.0, 4: 11.2}))
    if dropped or any(r["status"] == "FAIL" for r in rows):
        failures.append("a drop inside the measured noise was reported as a failure")
    if {r["status"] for r in rows} != {"unverified"}:
        failures.append(f"cells blinded by noise were not marked unverified: {[r['status'] for r in rows]}")

    # 9. A ragged sequential cell contaminates every row that divides by it, and does so in the
    #    flattering direction: the denominator grows, so the speedup looks better. It must be caught.
    ragged_base = suite({1: 30.0, 2: 15.0, 4: 8.0})
    ragged_cand = suite({1: 30.0, 2: 15.0, 4: 8.0})
    ragged_cand[("A", 4, 1)] = cell(36.0, spread=19.0)
    rows, _ = compare(ragged_base, ragged_cand)
    if any(r["status"] != "unverified" for r in rows):
        failures.append("a 19% spread on the denominator did not blind the rows above it")

    # 10. Half a suite is not a suite: 1 verified cell out of 4 exits 2, 3 of 4 exits 0.
    if verdict_exit([{"status": "ok"}] + [{"status": "unverified"}] * 3, []) != 2:
        failures.append("a mostly-unverifiable run was not rejected")
    if verdict_exit([{"status": "ok"}] * 3 + [{"status": "unverified"}], []) != 0:
        failures.append("a run with a noisy minority was rejected")
    if verdict_exit([{"status": "ok"}], [{"status": "FAIL"}]) != 1:
        failures.append("a failure did not take precedence over the verification share")

    # 11. A cell can be wide without blinding: 10% spread leaves the fail line at 25%.
    tight = suite({1: 30.0, 2: 15.0, 4: 8.0}, spread=10.0)
    rows, _ = compare(tight, suite({1: 30.0, 2: 15.0, 4: 8.0}, spread=10.0))
    if any(r["status"] != "ok" for r in rows):
        failures.append("a 10% spread was treated as unverifiable")

    # 12. A core count is comparable only when both sides measured real cores at it. The baseline
    #     predates topology recording, so its width-4 row is a hardware-thread count and is not
    #     comparable with a candidate's width-4 row, which is four real cores.
    legacy = {**suite({1: 30.0, 2: 15.0}, cores=2), **suite({1: 30.0, 2: 15.0, 4: 8.0}, cores=4)}
    fresh = dict(legacy)
    if comparable_widths({"schema": 2}, legacy, {"schema": 3}, fresh) != {2}:
        failures.append("a baseline's hardware-thread width was treated as a count of real cores")
    if comparable_widths({"schema": 3}, fresh, {"schema": 3}, fresh) != {2, 4}:
        failures.append("a sweep that recorded its topology was not treated as real cores throughout")
    rows, dropped = compare(legacy, fresh, widths={2})
    if dropped or any(row["cores"] != 2 for row in rows):
        failures.append("a width that means different things on the two sides was compared anyway")
    if len(compare(legacy, fresh)[0]) != 3:
        failures.append("the unrestricted comparison lost a cell")

    # 13. Widths left out of the comparison are named, not dropped in silence.
    notes = coverage_notes(legacy, fresh, {"schema": 2}, {"schema": 3})
    if not any("hardware threads" in note for note in notes):
        failures.append("a baseline's hardware-thread widths were not named in the coverage notes")

    # 6. Missing core counts are skipped, not failed.
    sparse = {(("A", 2, 1)): cell(30.0), ("A", 2, 2): cell(15.0)}
    rows, dropped = compare(base, sparse)
    if dropped or rows:
        failures.append("a run with no shared core count reported comparisons")

    # 7. A different fixture cannot be compared, and says so rather than inventing a verdict.
    if fixture_key({"fixture": {"funcs_per_crate": 1500}}) == fixture_key({"fixture": {"funcs_per_crate": 300}}):
        failures.append("fixture keys collided")
    notes = coverage_notes(base, {("A", 99, 1): cell(1.0)})
    if not any("no core count in common" in n for n in notes):
        failures.append("disjoint core counts were not reported")

    # 8. Multi-threaded cells use the same speedup definition.
    threaded = {("B", 6, 1): cell(8.0, threads=6, tasks=3), ("B", 6, 3): cell(5.0, threads=6, tasks=3)}
    if abs((speedup(threaded, "B", 6, 3) or 0) - 1.6) > 1e-9:
        failures.append("six-thread speedup computed wrong")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    if failures:
        return 1
    print("self-test: thresholds, noise, coverage and fixture guards behave as specified")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("baseline", nargs="?", help="recorded sweep, usually docs/dev/tuning/data/scaling-sweep.json")
    parser.add_argument("candidate", nargs="?", help="sweep from this run")
    parser.add_argument("--fail-pct", type=float, default=DEFAULT_FAIL_PCT, help="speedup drop that fails the build")
    parser.add_argument("--warn-pct", type=float, default=DEFAULT_WARN_PCT, help="speedup drop worth a warning")
    parser.add_argument("--json", metavar="PATH", help="write the verdicts as JSON for the CI artifact")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()
    if not args.baseline or not args.candidate:
        parser.error("baseline and candidate are both required unless --self-test is given")

    base_meta, base_cells = load(args.baseline)
    cand_meta, cand_cells = load(args.candidate)

    if fixture_key(base_meta) != fixture_key(cand_meta):
        print(
            "refusing to compare: the two sweeps measured different work "
            f"(baseline fixture {fixture_key(base_meta)}, candidate {fixture_key(cand_meta)})",
            file=sys.stderr,
        )
        return 2

    rows, failures = compare(
        base_cells,
        cand_cells,
        args.fail_pct,
        args.warn_pct,
        widths=comparable_widths(base_meta, base_cells, cand_meta, cand_cells),
    )
    notes = coverage_notes(base_cells, cand_cells, base_meta, cand_meta)

    print(f"baseline : {args.baseline} ({base_meta.get('recorded', 'undated')}, {base_meta.get('nproc', '?')} cores)")
    print(f"candidate: {args.candidate} ({cand_meta.get('recorded', 'undated')}, {cand_meta.get('nproc', '?')} cores)")
    for note in notes:
        print(f"note     : {note}")
    print()
    if rows:
        print(f"{'shape':<6}{'cores':>6}{'conc':>6}{'baseline':>10}{'now':>8}{'delta':>9}{'noise':>8}  verdict")
        for row in rows:
            print(
                f"{row['shape']:<6}{row['cores']:>6}{row['concurrency']:>6}"
                f"{row['baseline_speedup']:>9.2f}x{row['candidate_speedup']:>7.2f}x"
                f"{row['delta_pct']:>8.1f}%{row['noise_pct']:>7.1f}%  {row['status']}"
            )
        print()

    warnings = [r for r in rows if r["status"] == "WARN"]
    unverified = [r for r in rows if r["status"] == "unverified"]
    print(
        f"{len(rows)} comparable cells: {len(rows) - len(warnings) - len(failures) - len(unverified)} held, "
        f"{len(warnings)} warned, {len(failures)} failed, {len(unverified)} unverifiable"
    )
    for row in unverified:
        print(
            f"note     : shape {row['shape']} {row['cores']} cores concurrency {row['concurrency']} is "
            f"unverifiable at {row['noise_pct']:.1f}% noise (failure line {row['fail_at_pct']:.1f}%); "
            "raise REPEATS for that cell",
            file=sys.stderr,
        )
    if failures:
        worst = min(failures, key=lambda r: r["delta_pct"])
        print(
            f"FAIL scaling regressed: shape {worst['shape']} on {worst['cores']} cores at "
            f"concurrency {worst['concurrency']} lost {abs(worst['delta_pct']):.1f}% of its speedup",
            file=sys.stderr,
        )
    elif not rows:
        print("FAIL nothing comparable: check CORES/CONC against the baseline's core counts", file=sys.stderr)

    if args.json:
        pathlib.Path(args.json).write_text(
            json.dumps(
                {
                    "baseline": args.baseline,
                    "candidate": args.candidate,
                    "baseline_meta": base_meta,
                    "candidate_meta": cand_meta,
                    "notes": notes,
                    "comparable_widths": sorted(
                        comparable_widths(base_meta, base_cells, cand_meta, cand_cells)
                    ),
                    "rows": rows,
                    "failures": failures,
                    "unverified": unverified,
                    "verdict": (
                        "fail"
                        if failures or not rows
                        else ("unverified" if unverified else ("warn" if warnings else "pass"))
                    ),
                },
                indent=2,
            )
            + "\n"
        )
        print(f"wrote {args.json}")

    code = verdict_exit(rows, failures)
    if code == 2:
        print(
            f"FAIL cannot verify: {len(unverified)} of {len(rows)} comparable cells were blinded by their own "
            "noise; raise REF_REPEATS or REPEATS",
            file=sys.stderr,
        )
    return code


if __name__ == "__main__":
    sys.exit(main())
