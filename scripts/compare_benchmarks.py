#!/usr/bin/env python3
"""Put cross-engine JSON lines into one table.

    python3 scripts/compare_benchmarks.py results/*.json

Reads any file containing one JSON object per line with the keys docs/benchmarks/cross-engine.md defines, and prints
median, p95, p99, max and the over-budget share per (engine, agents), lowest median first. Refuses to compare rows that
were measured on different machines unless --force is passed, because that comparison is the one this whole harness
exists to prevent.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import sys

REQUIRED = (
    "engine",
    "agents",
    "median_ms",
    "p95_ms",
    "p99_ms",
    "max_ms",
    "over_budget_pct",
    "budget_ms",
    "machine",
)


def rows(paths: list[str]) -> list[dict]:
    found: list[dict] = []
    for path in paths:
        text = pathlib.Path(path).read_text()
        for line in text.splitlines():
            line = line.strip()
            # A harness may print the object bare, or tag it so a noisy log stays readable, or have the engine put its
            # own log prefix in front of the tag. All three are the same row.
            if line.startswith("{"):
                payload = line
            else:
                marker = line.find("crossbench {")
                if marker < 0:
                    continue
                payload = line[marker + len("crossbench ") :]
            try:
                row = json.loads(payload)
            except json.JSONDecodeError:
                continue
            missing = [key for key in REQUIRED if key not in row]
            if missing:
                print(f"{path}: skipping a row missing {', '.join(missing)}", file=sys.stderr)
                continue
            found.append(row)
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", help="JSON files, one object per line")
    parser.add_argument("--force", action="store_true", help="compare across different machines anyway")
    arguments = parser.parse_args()

    found = rows(arguments.paths)
    if not found:
        print("no benchmark rows found", file=sys.stderr)
        return 1

    machines = {row["machine"] for row in found}
    if len(machines) > 1 and not arguments.force:
        print("refusing to compare rows from different machines:", file=sys.stderr)
        for machine in sorted(machines):
            print(f"  {machine}", file=sys.stderr)
        print("re-run the harnesses on one machine, or pass --force and label the table as vendor-published.", file=sys.stderr)
        return 2

    found.sort(key=lambda row: (int(row["agents"]), float(row["median_ms"])))
    header = f"{'agents':>8}  {'engine':<20} {'median':>8} {'p95':>8} {'p99':>8} {'max':>8} {'>16.6ms':>8} {'ns/agent':>9}"
    print(header)
    print("-" * len(header))
    for row in found:
        per_agent = row.get("ns_per_agent")
        per_agent = (
            f"{per_agent:.0f}"
            if isinstance(per_agent, (int, float))
            else f"{float(row['median_ms']) * 1e6 / int(row['agents']):.0f}"
        )
        print(
            f"{int(row['agents']):>8}  {row['engine']:<20} "
            f"{float(row['median_ms']):>8.3f} {float(row['p95_ms']):>8.3f} "
            f"{float(row['p99_ms']):>8.3f} {float(row['max_ms']):>8.3f} "
            f"{float(row['over_budget_pct']):>7.2f}% {per_agent:>9}"
        )
    print()
    for machine in sorted(machines):
        print(f"machine: {machine}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
