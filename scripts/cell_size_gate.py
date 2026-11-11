#!/usr/bin/env python3
"""ADR 0031's re-open gate: is a finer grid worth a frame yet?

ADR 0031 decided to leave the grid budget at 2^20 cells (13.5 m in a one-kilometre volume) because a finer grid moved the
*kernel* by 8-20% and the *frame* by nothing. The reason was tiering: at 100,000 agents only a few thousand are manifested
and steered, so a kernel win on 5% of the crowd is inside the measurement's noise.

That decision has an expiry condition, and this script is it. If tiering ever raises the steered share, the frame will
start to care, and the numbers in `flock_frame_cell` will say so.

Two rules from ADR 0065 decide what a verdict means here, because the first version of this gate failed on `main` and
flipped between runs:

  * it judges the 100,000-agent case, the population ADR 0031 reasoned about. The 10,000-agent case is reported and
    cannot fail a run; it is a different question (tighter candidate boxes) asked with the same table.
  * it fails only on a difference it can resolve: the saving has to clear the fail threshold *and* the two cases' 95%
    intervals have to be disjoint. A saving above the line with overlapping intervals warns and passes, because that is a
    run to repeat rather than a decision to overturn.

So the verdicts are:

  * below the warn threshold          - the decision stands, exit 0
  * above warn, resolvable            - the frame is starting to care, annotate the run, exit 0
  * above fail, not resolvable        - repeat the run before believing it, exit 0
  * above fail, resolvable            - the decision is wrong, exit 2

Thresholds are percentages of whole-frame time that 9 m cells would save against 13.5 m, at 100,000 agents.

Usage:
    python3 scripts/cell_size_gate.py                    # read target/criterion, report
    python3 scripts/cell_size_gate.py --warn 3 --fail 10
    python3 scripts/cell_size_gate.py --chart            # ASCII breakdown, no gate
    python3 scripts/cell_size_gate.py --self-test        # fixtures, no criterion needed
"""

from __future__ import annotations

import argparse
import json
import pathlib
import tempfile
import sys

CRITERION = pathlib.Path("target/criterion")
GROUP = "flock_frame_cell"
# Criterion sanitises benchmark ids: `13.5m_2^20` becomes `13.5m_2_20`.
BASELINE = "13.5m_2_20"
CANDIDATE = "9m_2_21"
# ADR 0065: the gate judges the population ADR 0031's reasoning is about. The smaller crowd is reported, not judged.
JUDGED = "100000"
REPORTED = ("10000", "100000")
CONFIDENCE = 0.95


class Case:
    """One benchmark case: a mean frame time with the interval criterion measured around it."""

    __slots__ = ("population", "label", "mean_ms", "low_ms", "high_ms")

    def __init__(self, population: str, label: str, mean_ms: float, low_ms: float, high_ms: float) -> None:
        self.population = population
        self.label = label
        self.mean_ms = mean_ms
        self.low_ms = low_ms
        self.high_ms = high_ms


class Comparison:
    """One population's baseline against its candidate."""

    __slots__ = ("population", "base", "cand", "saving")

    def __init__(self, base: Case, cand: Case) -> None:
        self.population = base.population
        self.base = base
        self.cand = cand
        self.saving = 100.0 * (base.mean_ms - cand.mean_ms) / base.mean_ms if base.mean_ms > 0.0 else 0.0

    @property
    def resolvable(self) -> bool:
        """Whether the two intervals are disjoint, which is criterion's own test that a difference exists."""
        return self.base.low_ms > self.cand.high_ms or self.cand.low_ms > self.base.high_ms


def estimate(root: pathlib.Path, population: str, label: str) -> Case | None:
    """One benchmark case, if criterion produced it, with its 95% interval."""
    case = root / GROUP / population
    path = case / label / "new" / "estimates.json"
    if not path.exists():
        # Tolerate criterion's sanitising and any future label suffix.
        matches = sorted(case.glob(f"{label}*/new/estimates.json")) if case.exists() else []
        if not matches:
            return None
        path = matches[0]
    data = json.loads(path.read_text())
    mean = data["mean"]
    interval = mean["confidence_interval"]
    return Case(
        population,
        label,
        mean["point_estimate"] / 1_000_000.0,
        interval["lower_bound"] / 1_000_000.0,
        interval["upper_bound"] / 1_000_000.0,
    )


def rows(root: pathlib.Path) -> list[Comparison]:
    """Every population present, baseline against candidate."""
    found = []
    for population in REPORTED:
        base = estimate(root, population, BASELINE)
        cand = estimate(root, population, CANDIDATE)
        if base is None or cand is None:
            continue
        found.append(Comparison(base, cand))
    return found


def bar(value: float, peak: float, width: int = 24) -> str:
    if peak <= 0.0:
        return ""
    filled = round(width * max(0.0, value) / peak)
    return "#" * filled + "." * (width - filled)


def chart(cases: list[Comparison]) -> None:
    peak = max(max(case.base.high_ms, case.cand.high_ms) for case in cases)
    print("whole-frame time, 13.5 m (2^20 cells) against 9 m (2^21 cells)")
    print()
    print(f"{'agents':>8}  {'13.5 m':>15}  {'9 m':>15}  {'saving':>7}  {'judged':>8}")
    for case in cases:
        judged = "yes" if case.population == JUDGED else "reported"
        print(
            f"{case.population:>8}  "
            f"{case.base.mean_ms:>7.2f} ms {bar(case.base.mean_ms, peak)}  "
            f"{case.cand.mean_ms:>7.2f} ms {bar(case.cand.mean_ms, peak)}  "
            f"{case.saving:+6.1f}%  {judged:>8}"
        )
    print()
    print("95% intervals, milliseconds, as criterion measured them:")
    for case in cases:
        print(
            f"{case.population:>8}  13.5 m [{case.base.low_ms:7.2f}, {case.base.high_ms:7.2f}]   "
            f"9 m [{case.cand.low_ms:7.2f}, {case.cand.high_ms:7.2f}]   "
            f"{'disjoint' if case.resolvable else 'overlapping'}"
        )
    print()
    print("a longer bar is a slower frame; `judged` is the population the verdict is read from")


def verdict(cases: list[Comparison], warn: float, fail: float) -> tuple[int, list[str]]:
    """The gate's decision, as an exit code and the lines to print."""
    judged = next((case for case in cases if case.population == JUDGED), None)
    if judged is None:
        return 0, [
            f"cell-size gate: no {JUDGED}-agent case in {GROUP}; the gate judges that population and has nothing to read.",
            "Treating as a pass. A run that does not produce it is not evidence about ADR 0031.",
        ]
    saving = judged.saving
    if saving < warn:
        return 0, [
            f"PASS: 9 m cells save {saving:.1f}% of a {judged.population}-agent frame, below the {warn:.0f}% warn "
            "threshold.",
            "ADR 0031's decision stands.",
        ]
    if saving < fail:
        if judged.resolvable:
            return 0, [
                f"WARN: 9 m cells save {saving:.1f}% of a {judged.population}-agent frame, above the {warn:.0f}% warn "
                f"threshold but below {fail:.0f}%, and the intervals say the difference is real.",
                "The frame is starting to care. Watch it across a few runs before changing a default.",
            ]
        return 0, [
            f"WARN: 9 m cells appear to save {saving:.1f}% of a {judged.population}-agent frame, but the two intervals "
            "overlap.",
            "That is a run to repeat rather than a frame that has started to care.",
        ]
    if not judged.resolvable:
        return 0, [
            f"WARN: 9 m cells appear to save {saving:.1f}% of a {judged.population}-agent frame, above the {fail:.0f}% "
            "fail threshold, but the two 95% intervals overlap.",
            "This is the failure mode ADR 0065 was written for: a difference the harness cannot resolve. Repeat the run",
            "before treating ADR 0031 as overturned; nothing is failed on it.",
        ]
    return 2, [
        f"FAIL: 9 m cells save {saving:.1f}% of a {judged.population}-agent frame "
        f"({judged.base.mean_ms:.2f} ms -> {judged.cand.mean_ms:.2f} ms) with disjoint intervals, above the "
        f"{fail:.0f}% fail threshold.",
        "ADR 0031's decision to leave MAX_CELLS at 2^20 is wrong at this population: raise the default budget, re-record",
        "the numbers, and close the re-open trigger.",
    ]


def fixture(root: pathlib.Path, population: str, label: str, mean_ms: float, half_width_ms: float) -> None:
    """Write one criterion estimates file, so a verdict can be tested without running a benchmark."""
    directory = root / GROUP / population / label / "new"
    directory.mkdir(parents=True, exist_ok=True)
    payload = {
        "mean": {
            "confidence_interval": {
                "confidence_level": CONFIDENCE,
                "lower_bound": (mean_ms - half_width_ms) * 1_000_000.0,
                "upper_bound": (mean_ms + half_width_ms) * 1_000_000.0,
            },
            "point_estimate": mean_ms * 1_000_000.0,
            "standard_error": half_width_ms * 1_000_000.0 / 2.0,
        }
    }
    (directory / "estimates.json").write_text(json.dumps(payload))


def self_test() -> int:
    """Four fixtures: a resolvable fail, a fail-sized difference in the noise, an unjudged population, and nothing."""
    checks: list[tuple[str, bool]] = []

    def scenario(name: str, judged: tuple[float, float, float, float] | None, small: tuple[float, float, float, float] | None):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            if judged is not None:
                base, half, cand, chalf = judged
                fixture(root, JUDGED, BASELINE, base, half)
                fixture(root, JUDGED, CANDIDATE, cand, chalf)
            if small is not None:
                base, half, cand, chalf = small
                fixture(root, "10000", BASELINE, base, half)
                fixture(root, "10000", CANDIDATE, cand, chalf)
            cases = rows(root)
            return verdict(cases, 3.0, 10.0)

    code, lines = scenario("resolvable fail", (47.0, 0.5, 41.0, 0.5), None)
    checks.append(("a resolvable 13% saving fails", code == 2 and "FAIL" in lines[0]))

    code, lines = scenario("fail in the noise", (47.0, 4.0, 42.0, 4.0), None)
    text = " ".join(lines)
    checks.append(("a 13% saving inside the intervals warns and passes", code == 0 and "overlap" in text))

    code, lines = scenario("unjudged population", (47.0, 0.3, 47.0, 0.3), (6.0, 0.05, 5.1, 0.05))
    checks.append(("a 15% saving at 10,000 agents cannot fail the run", code == 0))

    code, lines = scenario("no results", None, None)
    checks.append(("no results is a pass with a reason", code == 0 and "nothing to read" in " ".join(lines)))

    code, lines = scenario("below warn", (47.0, 0.2, 46.0, 0.2), None)
    checks.append(("a 2% saving passes the decision", code == 0 and "decision stands" in " ".join(lines)))

    for label, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {label}")
    return 0 if all(ok for _, ok in checks) else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    # argparse runs help text through %-formatting, so a bare % must be doubled.
    # Python 3.14 validates the format eagerly and raises on a malformed sequence.
    parser.add_argument("--warn", type=float, default=3.0, help="warn above this saving (%%)")
    parser.add_argument("--fail", type=float, default=10.0, help="fail above this saving (%%)")
    parser.add_argument("--chart", action="store_true", help="print the ASCII breakdown only")
    parser.add_argument("--self-test", action="store_true", help="check the four verdicts against fixtures")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    cases = rows(CRITERION)
    if not cases:
        print(f"cell-size gate: no {GROUP} results found — treating as a pass (bench not run)")
        return 0

    chart(cases)
    if args.chart:
        return 0

    code, lines = verdict(cases, args.warn, args.fail)
    for line in lines:
        print(line)
    return code


if __name__ == "__main__":
    sys.exit(main())
