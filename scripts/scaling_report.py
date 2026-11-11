#!/usr/bin/env python3
"""Write the engineering review of a scaling sweep: curves, core-allocation efficiency, and whether a
candidate run regressed against the recorded baseline.

Everything in the generated report is computed from the sweep files; the prose describes what the
numbers do, so a reader who opens the report months later is not guessing which table to trust.

    python3 scripts/scaling_report.py docs/dev/tuning/data/scaling-sweep.json \\
        --candidate /tmp/candidate.json --out docs/dev/tuning/reports/scaling-report.md
"""

from __future__ import annotations

import argparse
import datetime
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from scaling_regression import (  # noqa: E402
    DEFAULT_FAIL_PCT,
    DEFAULT_WARN_PCT,
    FAIL_NOISE_MULTIPLIER,
    NOISE_FLOOR_PCT,
    WARN_NOISE_MULTIPLIER,
    compare,
    coverage_notes,
    load,
    speedup,
)

SHAPE_TITLES = {
    "A": "one crate per task (1 thread per task)",
    "B": "six crates per task (6 threads per task)",
}


def table(headers: list[str], rows: list[list[str]]) -> str:
    widths = [max(len(headers[i]), *(len(row[i]) for row in rows)) if rows else len(headers[i]) for i in range(len(headers))]
    line = "| " + " | ".join(headers[i].ljust(widths[i]) for i in range(len(headers))) + " |"
    rule = "| " + " | ".join("-" * widths[i] for i in range(len(headers))) + " |"
    body = ["| " + " | ".join(row[i].ljust(widths[i]) for i in range(len(headers))) + " |" for row in rows]
    return "\n".join([line, rule, *body])


def bars(pairs: list[tuple[str, float]], width: int = 32) -> str:
    peak = max(value for _, value in pairs) if pairs else 1.0
    lines = []
    for label, value in pairs:
        filled = max(1, round(value / peak * width)) if value > 0 else 0
        lines.append(f"{label:<28}{'#' * filled:<{width}} {value:5.2f}x")
    return "\n".join(lines)


def check_docs(path: str) -> int:
    """Regenerate in memory and compare. Prose that restates a threshold drifts; this catches it."""
    import subprocess
    import tempfile

    failures = []
    with tempfile.NamedTemporaryFile("r", suffix=".md", delete=False) as handle:
        scratch = handle.name
    subprocess.run(
        [sys.executable, __file__, "docs/dev/tuning/data/scaling-sweep.json", "--out", scratch],
        check=True,
        capture_output=True,
    )
    def without_timestamp(text: str) -> str:
        """The header records when the report was generated, which is expected to differ every run."""
        return "\n".join(line for line in text.splitlines() if not line.startswith("Generated "))

    committed = without_timestamp(pathlib.Path(path).read_text())
    fresh = without_timestamp(pathlib.Path(scratch).read_text())
    pathlib.Path(scratch).unlink()
    if committed != fresh:
        failures.append(
            f"{path} is stale: regenerate with `python3 scripts/scaling_report.py docs/dev/tuning/data/scaling-sweep.json`"
        )

    adr = pathlib.Path("docs/dev/adr/0034-scaling-regression-suite.md").read_text()
    import json

    records = [
        json.loads(line)
        for line in pathlib.Path("docs/dev/tuning/data/scaling-sweep.json").read_text().splitlines()
        if line.strip()
    ]
    spreads = [float(r.get("spread_pct") or 0.0) for r in records if r.get("kind") == "measurement"]
    widest = max([NOISE_FLOOR_PCT, *spreads])
    expected = {
        f"max({DEFAULT_FAIL_PCT:.0f}%, {FAIL_NOISE_MULTIPLIER:.1f} x widest)": "the failure formula",
        f"max({DEFAULT_WARN_PCT:.0f}%, {WARN_NOISE_MULTIPLIER:.1f} x widest)": "the warning formula",
        f"{max(DEFAULT_FAIL_PCT, FAIL_NOISE_MULTIPLIER * widest):.1f}%": "the effective failure line",
        f"{max(DEFAULT_WARN_PCT, WARN_NOISE_MULTIPLIER * widest):.1f}%": "the effective warning line",
    }
    for snippet, label in expected.items():
        if snippet not in adr:
            failures.append(f"ADR 0034 no longer states {label} ({snippet})")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    if failures:
        return 1
    print("docs ok: report matches the baseline, ADR 0034 matches the gate's constants")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("sweep", nargs="?", help="sweep file to report on")
    parser.add_argument("--candidate", help="a newer sweep to compare against the baseline")
    parser.add_argument("--out", default="docs/dev/tuning/reports/scaling-report.md")
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify the committed report and ADR 0034 still match the baseline and the gate's constants",
    )
    args = parser.parse_args()

    if args.check:
        return check_docs(args.out)
    if not args.sweep:
        parser.error("a sweep file is required unless --check is given")

    meta, cells = load(args.sweep)
    shapes = sorted({key[0] for key in cells})
    cores_list = sorted({key[1] for key in cells})
    fixture = meta.get("fixture", {})
    recorded = meta.get("recorded", "undated")
    nproc = meta.get("nproc", "?")
    machine = meta.get("machine", "unknown")
    processor = meta.get("processor", "unknown")
    repeats = meta.get("repeats", 1)
    max_spread = max((cell["spread"] for cell in cells.values()), default=0.0)

    out: list[str] = []
    add = out.append

    add("# Scaling report: cores, threads per task, and what the crawl fills")
    add("")
    add(f"Generated {datetime.datetime.now().astimezone().isoformat(timespec='seconds')} from "
        f"`{args.sweep}`, recorded {recorded} on {nproc} cores ({processor}, `{machine}`).")
    add("")
    add("Every number here is measured: each run is pinned with `taskset`, each task gets a cold target "
        f"directory, and each cell is the median of {repeats} runs. Nothing is modelled. The fixture is "
        f"{fixture.get('crates_one_per_task', '?')} crates of {fixture.get('funcs_per_crate', '?')} functions "
        f"for shape A, and {fixture.get('bundles', '?')} bundles of {fixture.get('crates_per_bundle', '?')} "
        "crates for shape B.")
    add("")
    add("![Wall clock and speedup against core count](../../../images/scaling-curves.png)")
    add("")
    add("Two quantities are used throughout. **Speedup** is the same core count's sequential wall clock "
        "divided by the wall clock at that concurrency: dimensionless, so it compares across machines. "
        "**Efficiency** is that speedup divided by `min(cores, concurrency x threads per task)`: the "
        "fraction of the ideal the run actually delivered. Above 100% is possible and is explained where "
        "it happens.")
    add("")

    # The gate's own numbers, computed here rather than restated in prose: an earlier draft of this
    # report said "below 18%" while the gate failed at 25%, and a document that disagrees with its gate
    # teaches readers to distrust both.
    widest = max(NOISE_FLOOR_PCT, max_spread)
    warn_effective = max(DEFAULT_WARN_PCT, WARN_NOISE_MULTIPLIER * widest)
    fail_effective = max(DEFAULT_FAIL_PCT, FAIL_NOISE_MULTIPLIER * widest)
    add("## Tolerances this report is judged by")
    add("")
    add(f"ADR 0034 commits to a **warning at {DEFAULT_WARN_PCT:.0f}%** and a **failure at "
        f"{DEFAULT_FAIL_PCT:.0f}%** of lost speedup, each widened by the measured spread "
        f"({WARN_NOISE_MULTIPLIER:.1f}x for a warning, {FAIL_NOISE_MULTIPLIER:.1f}x for a failure). The "
        f"widest spread on this baseline is {max_spread:.1f}%, so the gate applies "
        f"**{warn_effective:.1f}%** and **{fail_effective:.1f}%**.")
    add("")
    ci_shape = "A" if "A" in shapes else shapes[0]
    concs_for_shape = sorted({key[2] for key in cells if key[0] == ci_shape})
    ci_conc = 4 if 4 in concs_for_shape else concs_for_shape[-1]
    ci_key = (ci_shape, max(cores_list), ci_conc)
    if ci_key in cells:
        sequential = cells[(ci_shape, max(cores_list), 1)]["wall"]
        base_speed = speedup(cells, ci_shape, max(cores_list), ci_conc)
        warn_wall = sequential / (base_speed * (1 - warn_effective / 100))
        fail_wall = sequential / (base_speed * (1 - fail_effective / 100))
        add(f"In wall clock, which is what a developer feels: the CI's {ci_conc}-at-once cell on "
            f"{max(cores_list)} cores runs in {cells[ci_key]['wall']:.1f} s today, warns above "
            f"{warn_wall:.1f} s and fails above {fail_wall:.1f} s. The second tolerance this suite owes is "
            "the CI budget itself: the job carries a 15-minute timeout against a measured three minutes on "
            "a six-core machine.")
        add("")
    add("Runtime frame budgets (16.6 ms at 10,000 agents, 8 ms at 100,000 with 5,000 manifested, 6 ms at "
        "1,000,000) are enforced by the criterion benches and `docs/dev/meta/release-checklist.md`, not "
        "here: this suite grades parallelism, not absolute speed.")
    add("")

    for shape in shapes:
        threads = next(cell["threads"] for key, cell in cells.items() if key[0] == shape)
        tasks = next(cell["tasks"] for key, cell in cells.items() if key[0] == shape)
        concurrencies = sorted({key[2] for key in cells if key[0] == shape})
        add(f"## Shape {shape}: {SHAPE_TITLES.get(shape, shape)}, {tasks} tasks")
        add("")
        header = ["cores"] + [f"{c} at once" for c in concurrencies]
        walls = []
        for cores in cores_list:
            row = [str(cores)]
            for concurrency in concurrencies:
                cell = cells.get((shape, cores, concurrency))
                row.append(f"{cell['wall']:.1f} s" if cell else "-")
            walls.append(row)
        add("Wall clock:")
        add("")
        add(table(header, walls))
        add("")
        speedups = []
        for cores in cores_list:
            row = [str(cores)]
            for concurrency in concurrencies:
                value = speedup(cells, shape, cores, concurrency)
                row.append(f"{value:.2f}x" if value else "-")
            speedups.append(row)
        add("Speedup against that core count's sequential run:")
        add("")
        add(table(header, speedups))
        add("")
        efficiency_rows = []
        for cores in cores_list:
            row = [str(cores)]
            for concurrency in concurrencies:
                value = speedup(cells, shape, cores, concurrency)
                ideal = min(cores, concurrency * threads)
                row.append(f"{value / ideal * 100:.0f}%" if value else "-")
            efficiency_rows.append(row)
        add("Efficiency, as a percentage of `min(cores, concurrency x threads)`:")
        add("")
        add(table(header, efficiency_rows))
        add("")

    add("## Where the crawl fills, against what the cap formula would choose")
    add("")
    rows = []
    for shape in shapes:
        threads = next(cell["threads"] for key, cell in cells.items() if key[0] == shape)
        for cores in cores_list:
            candidates = [
                (concurrency, speedup(cells, shape, cores, concurrency))
                for concurrency in sorted({key[2] for key in cells if key[0] == shape})
            ]
            candidates = [(c, s) for c, s in candidates if s]
            if not candidates:
                continue
            best_concurrency, best_speedup = max(candidates, key=lambda pair: pair[1])
            cap = max(1, cores // threads)
            rows.append([
                shape,
                str(cores),
                str(cap),
                str(best_concurrency),
                f"{best_speedup:.2f}x",
                f"{best_concurrency / cap:.2f}x",
            ])
    add(table(["shape", "cores", "cap formula says", "best measured", "speedup at best", "best ÷ cap"], rows))
    add("")
    add("The last column is the honest score for ADR 0033's rule: above 1.0 the formula admits fewer tasks "
        "than the machine rewards, below 1.0 it admits more than help.")
    add("")

    add("## Best measured speedup per core count")
    add("")
    peaks = []
    for shape in shapes:
        for cores in cores_list:
            candidates = [
                (concurrency, speedup(cells, shape, cores, concurrency))
                for concurrency in sorted({key[2] for key in cells if key[0] == shape})
            ]
            candidates = [(c, s) for c, s in candidates if s]
            if candidates:
                peaks.append((f"{shape} {cores}c", max(s for _, s in candidates)))
    add("```")
    add(bars(peaks))
    add("```")
    add("")

    add("## Findings")
    add("")
    findings: list[str] = []
    findings.append(
        f"Repeats differed by at most {max_spread:.1f}% on any cell; the gate's tolerance is "
        f"{fail_effective:.1f}% to fail and {warn_effective:.1f}% to warn, both computed above."
    )
    findings.append(
        "This suite gates the *shape* of the curve, not absolute speed: speedups are ratios against the "
        "same run's sequential baseline, so a change that slows every task equally cancels out. Per-task "
        "cost regressions belong to the criterion benches (`crowd`, `packed`, `brain`, `tiers`, `band`), "
        "which compare absolute times on one machine (ADR 0034)."
    )
    for shape in shapes:
        threads = next(cell["threads"] for key, cell in cells.items() if key[0] == shape)
        largest = max(cores_list)
        candidates = [
            (concurrency, speedup(cells, shape, largest, concurrency))
            for concurrency in sorted({key[2] for key in cells if key[0] == shape})
        ]
        candidates = [(c, s) for c, s in candidates if s]
        if not candidates:
            continue
        best_concurrency, best_speedup = max(candidates, key=lambda pair: pair[1])
        cap = max(1, largest // threads)
        at_cap = dict(candidates).get(cap)
        if at_cap is None:
            continue
        gain = (best_speedup / at_cap - 1.0) * 100.0 if at_cap else 0.0
        # The direction of the advice depends on the measurement, not on the shape's label. A bundle
        # that already fills the cores gains nothing from company; a bursty one-task compile does.
        if gain >= 10.0:
            findings.append(
                f"Shape {shape} on {largest} cores: the formula admits {cap} and the measured best is "
                f"{best_concurrency} ({best_speedup:.2f}x against {at_cap:.2f}x at the formula's choice, "
                f"+{gain:.0f}%). The formula under-admits this shape."
            )
        elif gain <= 3.0:
            findings.append(
                f"Shape {shape} on {largest} cores: the formula's {cap} is essentially the measured best "
                f"({at_cap:.2f}x against {best_speedup:.2f}x at {best_concurrency}, "
                f"{gain:+.0f}%). More concurrency is within noise — this shape already fills the cores it "
                "is given."
            )
        else:
            findings.append(
                f"Shape {shape} on {largest} cores: the formula admits {cap} ({at_cap:.2f}x) and the "
                f"measured best is {best_concurrency} ({best_speedup:.2f}x, {gain:+.0f}%), so the rule is "
                "close but not optimal for this shape."
            )

    # Threads versus tasks: does packing threads into one task already use the machine?
    for cores in cores_list:
        alone = speedup(cells, "B", cores, 1) if ("B", cores, 1) in cells else None
        concurrencies = sorted({key[2] for key in cells if key[0] == "B"})
        together = speedup(cells, "B", cores, concurrencies[-1]) if concurrencies else None
        if alone and together and cores >= 3:
            change = (together / alone - 1.0) * 100.0
            if change >= 10.0:
                findings.append(
                    f"On {cores} cores, three six-thread bundles reach {together:.2f}x where one alone "
                    f"reaches {alone:.2f}x ({change:+.0f}%), so admitting one at a time would leave the "
                    "machine partly idle."
                )
            else:
                findings.append(
                    f"On {cores} cores, one six-thread bundle alone reaches {alone:.2f}x and three together "
                    f"reach {together:.2f}x ({change:+.0f}%): the threads inside the task already fill the "
                    "machine, which is why the policy counts threads rather than tasks."
                )
            break

    for finding in findings:
        add(f"- {finding}")
    add("")

    if args.candidate:
        cand_meta, cand_cells = load(args.candidate)
        rows, failures = compare(cells, cand_cells)
        warnings = [row for row in rows if row["status"] == "WARN"]
        add("## Regression check against a newer run")
        add("")
        add(f"Candidate: `{args.candidate}`, recorded {cand_meta.get('recorded', 'undated')} on "
            f"{cand_meta.get('nproc', '?')} cores. Comparison is on speedup, so a slower machine is not a "
            "regression.")
        for note in coverage_notes(cells, cand_cells):
            add(f"- {note}")
        add("")
        add(table(
            ["shape", "cores", "conc", "baseline", "now", "delta", "noise", "verdict"],
            [[r["shape"], str(r["cores"]), str(r["concurrency"]), f"{r['baseline_speedup']:.2f}x",
              f"{r['candidate_speedup']:.2f}x", f"{r['delta_pct']:+.1f}%", f"{r['noise_pct']:.0f}%",
              r["status"]] for r in rows],
        ))
        add("")
        verdict = "FAIL" if failures else ("WARN" if warnings else "PASS")
        add(f"Verdict: **{verdict}** over {len(rows)} comparable cells "
            f"({len(failures)} failed, {len(warnings)} warned).")
        add("")

    add("## Reproducing")
    add("")
    add("```bash")
    add("scripts/scaling_sweep.sh /tmp/sweep.json            # ~20 minutes, pinned, two repeats per cell")
    add("python3 scripts/scaling_curves.py /tmp/sweep.json docs/images/scaling-curves.png")
    add("python3 scripts/scaling_regression.py docs/dev/tuning/data/scaling-sweep.json /tmp/sweep.json")
    add("python3 scripts/scaling_report.py docs/dev/tuning/data/scaling-sweep.json --out docs/dev/tuning/reports/scaling-report.md")
    add("python3 scripts/scaling_report.py --check                       # fails if this report or ADR 0034 drifted")
    add("```")
    add("")
    add("CI runs a reduced matrix (`SHAPES=\"A\" CORES=\"1 $(nproc)\" CONC=\"1 2 4\"`, two repeats) on every "
        "push and compares it with the same baseline; the full matrix above stays a local or nightly job "
        "because it costs twenty minutes.")
    add("")

    path = pathlib.Path(args.out)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(out))
    print(f"wrote {path} ({len(out)} lines)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
