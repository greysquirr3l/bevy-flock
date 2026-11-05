#!/usr/bin/env python3
"""Write the engineering review of a scaling sweep: curves, core-allocation efficiency, and whether a
candidate run regressed against the recorded baseline.

Everything in the generated report is computed from the sweep files; the prose describes what the
numbers do, so a reader who opens the report months later is not guessing which table to trust.

    python3 scripts/scaling_report.py docs/data/scaling-sweep.json \\
        --candidate /tmp/candidate.json --out docs/reports/scaling-report.md
"""

from __future__ import annotations

import argparse
import datetime
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from scaling_regression import compare, coverage_notes, load, speedup  # noqa: E402

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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("sweep")
    parser.add_argument("--candidate", help="a newer sweep to compare against the baseline")
    parser.add_argument("--out", default="docs/reports/scaling-report.md")
    args = parser.parse_args()

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
    add("![Wall clock and speedup against core count](../images/scaling-curves.png)")
    add("")
    add("Two quantities are used throughout. **Speedup** is the same core count's sequential wall clock "
        "divided by the wall clock at that concurrency: dimensionless, so it compares across machines. "
        "**Efficiency** is that speedup divided by `min(cores, concurrency x threads per task)`: the "
        "fraction of the ideal the run actually delivered. Above 100% is possible and is explained where "
        "it happens.")
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
    spread_note = (
        f"Repeats differed by at most {max_spread:.1f}% on any cell, so differences below "
        f"{max(8.0, 2 * max_spread):.0f}% are not treated as regressions."
    )
    findings.append(spread_note)
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
    add("python3 scripts/scaling_regression.py docs/data/scaling-sweep.json /tmp/sweep.json")
    add("python3 scripts/scaling_report.py docs/data/scaling-sweep.json --out docs/reports/scaling-report.md")
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
