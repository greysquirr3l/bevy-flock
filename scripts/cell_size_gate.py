#!/usr/bin/env python3
"""ADR 0031's re-open gate: is a finer grid worth a frame yet?

ADR 0031 decided to leave the grid budget at 2^20 cells (13.5 m in a one-kilometre volume) because
a finer grid moved the *kernel* by 8-20% and the *frame* by nothing. The reason was tiering: at
100,000 agents only a few thousand are manifested and steered, so a kernel win on 5% of the crowd
is inside the measurement's noise.

That decision has an expiry condition, and this script is it. If tiering ever raises the steered
share, the frame will start to care, and the numbers in `flock_frame_cell` will say so. CI runs the
benchmark and this script on every push:

  * below the warn threshold  - the decision stands, exit 0
  * above warn, below fail    - the frame is starting to care, annotate the run, exit 0
  * above the fail threshold  - the decision is wrong, exit 2

Thresholds are percentages of whole-frame time that 9 m cells would save against 13.5 m. The warn
band exists because criterion on a shared CI runner cannot resolve 3% honestly; the gate fails only
on a difference that a noisy runner can still see.

Usage:
    python3 scripts/cell_size_gate.py                    # read target/criterion, report
    python3 scripts/cell_size_gate.py --warn 3 --fail 10
    python3 scripts/cell_size_gate.py --chart            # ASCII breakdown, no gate
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

CRITERION = pathlib.Path("target/criterion")
GROUP = "flock_frame_cell"
# Criterion sanitises benchmark ids: `13.5m_2^20` becomes `13.5m_2_20`.
BASELINE = "13.5m_2_20"
CANDIDATE = "9m_2_21"


def estimate(population: str, label: str) -> float | None:
    """Mean frame time in milliseconds for one benchmark case, if criterion produced it."""
    case = CRITERION / GROUP / population
    path = case / label / "new" / "estimates.json"
    if not path.exists():
        # Tolerate criterion's sanitising and any future label suffix.
        matches = sorted(case.glob(f"{label}*/new/estimates.json")) if case.exists() else []
        if not matches:
            return None
        path = matches[0]
    data = json.loads(path.read_text())
    return data["mean"]["point_estimate"] / 1_000_000.0


def rows() -> list[tuple[str, float, float, float]]:
    """(population, baseline ms, candidate ms, saving %) for every case present."""
    found = []
    for population in ("10000", "100000"):
        base = estimate(population, BASELINE)
        cand = estimate(population, CANDIDATE)
        if base is None or cand is None:
            continue
        found.append((population, base, cand, 100.0 * (base - cand) / base))
    return found


def bar(value: float, peak: float, width: int = 28) -> str:
    if peak <= 0.0:
        return ""
    filled = round(width * max(0.0, value) / peak)
    return "#" * filled + "." * (width - filled)


def chart() -> None:
    cases = rows()
    if not cases:
        print(f"no {GROUP} results in {CRITERION}; run the benchmark first:")
        print("  cargo bench --bench crowd -- flock_frame_cell")
        return
    peak = max(max(base, cand) for _, base, cand, _ in cases)
    print("whole-frame time, 13.5 m (2^20 cells) against 9 m (2^21 cells)")
    print()
    print(f"{'agents':>8}  {'13.5 m':>9}  {'':<28}  {'9 m':>9}  {'':<28}  saving")
    for population, base, cand, saving in cases:
        print(
            f"{population:>8}  {base:>7.2f}ms  {bar(base, peak)}  "
            f"{cand:>7.2f}ms  {bar(cand, peak)}  {saving:+5.1f}%"
        )
    print()
    print("a longer bar is a slower frame; the saving column is what the finer grid would buy")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    # argparse runs help text through %-formatting, so a bare % must be doubled.
    # Python 3.14 validates the format eagerly and raises on a malformed sequence.
    parser.add_argument("--warn", type=float, default=3.0, help="warn above this saving (%%)")
    parser.add_argument("--fail", type=float, default=10.0, help="fail above this saving (%%)")
    parser.add_argument("--chart", action="store_true", help="print the ASCII breakdown only")
    args = parser.parse_args()

    cases = rows()
    if not cases:
        print(f"cell-size gate: no {GROUP} results found — treating as a pass (bench not run)")
        return 0

    chart()
    if args.chart:
        return 0

    worst = max(cases, key=lambda case: case[3])
    population, base, cand, saving = worst
    if saving >= args.fail:
        print(
            f"FAIL: 9 m cells save {saving:.1f}% of a {population}-agent frame "
            f"({base:.2f} ms -> {cand:.2f} ms), above the {args.fail:.0f}% fail threshold.\n"
            "ADR 0031's decision to leave MAX_CELLS at 2^20 is wrong at this population: raise the\n"
            "default budget, re-record the numbers, and close the re-open trigger."
        )
        return 2
    if saving >= args.warn:
        print(
            f"WARN: 9 m cells save {saving:.1f}% of a {population}-agent frame, above the "
            f"{args.warn:.0f}% warn threshold but below {args.fail:.0f}%.\n"
            "The frame is starting to care. Watch it across a few runs before changing a default."
        )
        return 0
    print(
        f"PASS: the best saving is {saving:.1f}% at {population} agents, below the "
        f"{args.warn:.0f}% warn threshold. ADR 0031's decision stands."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
