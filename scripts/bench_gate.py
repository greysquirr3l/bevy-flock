#!/usr/bin/env python3
"""Fail a build when the animation binding regresses against its committed baseline.

The benchmark in `crates/bevy-flock-anim/benches/binding.rs` reports one number per case. This
compares each against `crates/bevy-flock-anim/benches/baseline.json` and fails when a case is more
than `--tolerance` slower (25% by default, which is the gate the phase-2 plan asks for).

Two ways to use it:

    python3 scripts/bench_gate.py                 # run the bench, compare, exit non-zero on a regression
    python3 scripts/bench_gate.py --check FILE    # compare an already-captured run
    python3 scripts/bench_gate.py --write-baseline # re-record the baseline (say so in the commit)

The baseline is recorded on one machine and compared on another, so the comparison is a ratio, never
an absolute. A machine 30% slower than the one that recorded the baseline fails every case; that is
why the file records the machine it was taken on and why the gate is meant for CI on a fixed runner.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
# Two gates share this script: the animation binding (`crates/bevy-flock-anim`, bench `binding`) and
# the serial spatial path (`bevy-flock`, bench `spatial`, phase 3's baseline). Each crate owns its own
# baseline file, because a baseline is only meaningful against the machine that recorded it and the
# crate that changed.
DEFAULT_CRATE = ROOT / "crates" / "bevy-flock-anim"
DEFAULT_BENCH = "binding"
LINE = re.compile(
    # Any case prefix: the animation bench names its cases `binding/...`, the spatial one
    # `spatial_build/...`, and a future bench will name its own. Parsing the prefix would mean editing
    # this file per bench, and forgetting to is how a gate silently compares nothing.
    r"^test (?P<case>\S+) \.\.\. bench:\s+(?P<nanos>[\d,]+) ns/iter \((?P<var>.*)\)$"
)


def run_bench(crate: pathlib.Path, bench: str) -> dict[str, int]:
    """Run the bench and parse its bencher-format output into `{case: nanoseconds}`."""
    completed = subprocess.run(
        ["cargo", "bench", "--offline", "--bench", bench, "--", "--output-format", "bencher"],
        cwd=crate,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        print(completed.stdout[-4000:], file=sys.stderr)
        print(completed.stderr[-4000:], file=sys.stderr)
        raise SystemExit("the benchmark itself failed to run")
    results: dict[str, int] = {}
    for line in completed.stdout.splitlines():
        match = LINE.match(line)
        if match:
            results[match.group("case")] = int(match.group("nanos").replace(",", ""))
    if not results:
        raise SystemExit("the benchmark ran but reported no cases")
    # Keep the capture even if the comparison or the write fails afterwards: a twelve-minute run that
    # ends in a stack trace should not have to be repeated to recover its numbers.
    capture = pathlib.Path(tempfile.gettempdir()) / f"bench-capture-{bench}.json"
    capture.write_text(json.dumps(results, indent=2) + "\n")
    return results


def load(path: pathlib.Path) -> dict:
    with path.open() as handle:
        return json.load(handle)


def compare(current: dict[str, int], baseline: dict) -> int:
    """Print a table and return the number of regressions."""
    cases = baseline.get("cases", {})
    failures = 0
    print(f"{'case':<46}{'baseline':>12}{'current':>12}{'change':>10}")
    for case, recorded in sorted(cases.items()):
        if case not in current:
            print(f"{case:<46}{recorded:>12,}{'missing':>12}{'':>10}")
            failures += 1
            continue
        now = current[case]
        change = (now - recorded) / recorded
        flag = "  REGRESSED" if change > baseline.get("tolerance", 0.25) else ""
        print(f"{case:<46}{recorded:>12,}{now:>12,}{change:>+9.1%}{flag}")
        if flag:
            failures += 1
    for case in sorted(set(current) - set(cases)):
        print(f"{case:<46}{'-':>12}{current[case]:>12,}{'new':>10}")
    if not cases:
        print("the baseline has no cases; record one with --write-baseline", file=sys.stderr)
        return 1
    return failures


def write_baseline(current: dict[str, int], tolerance: float, machine: str, path: pathlib.Path) -> None:
    payload = {
        "machine": machine,
        "tolerance": tolerance,
        "caveat": (
            "Recorded on one machine; the gate compares ratios, so run it on the machine that owns "
            "this file. See docs/performance.md for what each case is."
        ),
        "cases": dict(sorted(current.items())),
    }
    path.write_text(json.dumps(payload, indent=2) + "\n")


def machine_name() -> str:
    model = "unknown"
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text().splitlines():
            if line.startswith("model name"):
                model = line.split(":", 1)[1].strip()
                break
    return f"{model} ({len(__import__('os').sched_getaffinity(0))} cores)"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--crate", type=pathlib.Path, default=DEFAULT_CRATE, help="crate whose bench to run")
    parser.add_argument("--bench", default=DEFAULT_BENCH, help="bench target name")
    parser.add_argument("--baseline", type=pathlib.Path, help="baseline file; defaults beside the bench")
    parser.add_argument("--check", type=pathlib.Path, help="compare a captured run instead of running")
    parser.add_argument("--write-baseline", action="store_true", help="record the current run")
    parser.add_argument("--tolerance", type=float, default=0.25, help="allowed slowdown, as a fraction")
    options = parser.parse_args()

    options.crate = options.crate.resolve()
    if options.baseline is not None:
        options.baseline = options.baseline.resolve()

    if options.check is not None:
        captured = load(options.check)
        # Accept either a flat capture or a baseline-shaped file, so a recorded baseline can be
        # re-checked and so an older capture still compares.
        cases = captured.get("cases", captured)
        current = {key: int(value) for key, value in cases.items()}
    else:
        current = run_bench(options.crate, options.bench)

    baseline_path = options.baseline or options.crate / "benches" / "baseline.json"
    baseline_path = baseline_path.resolve()

    if options.write_baseline:
        write_baseline(current, options.tolerance, machine_name(), baseline_path)
        try:
            shown = baseline_path.relative_to(ROOT)
        except ValueError:
            shown = baseline_path
        print(f"wrote {shown} with {len(current)} cases")
        return 0

    if not baseline_path.exists():
        print(f"no baseline at {baseline_path.relative_to(ROOT)}; record one with --write-baseline", file=sys.stderr)
        return 1
    baseline = load(baseline_path)
    baseline["tolerance"] = options.tolerance
    print(f"baseline: {baseline.get('machine', 'unknown')}")
    failures = compare(current, baseline)
    if failures:
        print(f"\n{failures} case(s) regressed by more than {options.tolerance:.0%}", file=sys.stderr)
        return 1
    print(f"\nno case regressed by more than {options.tolerance:.0%}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
