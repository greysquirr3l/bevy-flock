#!/usr/bin/env python3
"""Run the cross-engine fixture on this machine and print the table.

    python3 scripts/cross_bench.py --agents 10000,100000
    python3 scripts/cross_bench.py --agents 10000 --engines bevy-flock,godot-grid --out /tmp/bench

Item 7 of the 1.0 checklist is "comparable performance, on one machine", and the hard part was never the
measuring: it was remembering six commands, two of which belong to a different engine, and getting the same
agent count and tick count into both. This script is that memory. It runs each harness with identical
arguments, writes one JSON line per row, then hands the files to `compare_benchmarks.py`, whose refusal to
compare across machines is the reason the runner never merges results from different runs.

Engines it knows about:

  bevy-flock   examples/cross_bench.rs, release build, `App::update` per tick
  godot-array  the GDScript fixture with no spatial index, a lower bound for Godot
  godot-grid   the same fixture plus a uniform-grid rebuild, the like-for-like row
  unreal       not runnable here; the harness is described in docs/dev/tuning/benchmarks/cross-engine.md

A missing engine is skipped with a note rather than a failure, because a machine without Godot can still
measure our side, and a run that silently reported nothing would be worse than one that says why.
"""

from __future__ import annotations

import argparse
import pathlib
import shutil
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
GODOT_DIR = REPO / "benchmarks" / "godot"
ENGINES = ("bevy-flock", "godot-array", "godot-grid")


def run(command: list[str], cwd: pathlib.Path | None = None) -> str:
    """Run a harness and return its stdout, refusing to hide a non-zero exit."""
    completed = subprocess.run(command, cwd=cwd, capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        raise SystemExit(
            f"command failed ({completed.returncode}): {' '.join(command)}\n{completed.stderr.strip()[:2000]}"
        )
    return completed.stdout


def bevy_rows(agents: list[int], ticks: int, warmup: int) -> list[str]:
    example = REPO / "target" / "release" / "examples" / "cross_bench"
    if not example.exists():
        raise SystemExit(
            f"{example} is missing: build it first with\n"
            "  cargo build --release --example cross_bench"
        )
    joined = ",".join(str(count) for count in agents)
    output = run([str(example), "--agents", joined, "--ticks", str(ticks), "--warmup", str(warmup), "--json"])
    return [line for line in output.splitlines() if line.startswith("{")]


def godot_rows(agents: list[int], ticks: int, warmup: int, mode: str, godot: str) -> list[str]:
    rows = []
    for count in agents:
        output = run(
            [
                godot,
                "--headless",
                "--path",
                str(GODOT_DIR),
                "--",
                "--agents",
                str(count),
                "--ticks",
                str(ticks),
                "--warmup",
                str(warmup),
                "--mode",
                mode,
            ],
            cwd=GODOT_DIR,
        )
        rows.extend(line[len("crossbench ") :] for line in output.splitlines() if line.startswith("crossbench "))
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--agents", default="10000,100000", help="comma-separated agent counts")
    parser.add_argument("--ticks", type=int, default=1000, help="measured ticks per row")
    parser.add_argument("--warmup", type=int, default=100, help="unmeasured ticks before the measured ones")
    parser.add_argument("--engines", default=",".join(ENGINES), help=f"any of {', '.join(ENGINES)}")
    parser.add_argument("--out", default="", help="directory for the JSONL files (default: alongside the run)")
    arguments = parser.parse_args()

    counts = [int(value) for value in arguments.agents.split(",") if value.strip()]
    wanted = [name.strip() for name in arguments.engines.split(",") if name.strip()]
    unknown = [name for name in wanted if name not in ENGINES]
    if unknown:
        raise SystemExit(f"unknown engine(s): {', '.join(unknown)}; known: {', '.join(ENGINES)}")

    out_dir = pathlib.Path(arguments.out) if arguments.out else REPO / "target" / "cross-bench"
    out_dir.mkdir(parents=True, exist_ok=True)

    written: list[pathlib.Path] = []
    if "bevy-flock" in wanted:
        path = out_dir / "bevy-flock.jsonl"
        path.write_text("\n".join(bevy_rows(counts, arguments.ticks, arguments.warmup)) + "\n")
        written.append(path)

    godot = shutil.which("godot") or shutil.which("godot4")
    godot_wanted = [name for name in wanted if name.startswith("godot-")]
    if godot_wanted and not godot:
        print("godot is not on PATH: skipping the Godot rows rather than reporting a one-engine table", file=sys.stderr)
    elif godot:
        for name in godot_wanted:
            mode = name.removeprefix("godot-")
            path = out_dir / f"{name}.jsonl"
            path.write_text("\n".join(godot_rows(counts, arguments.ticks, arguments.warmup, mode, godot)) + "\n")
            written.append(path)

    if not written:
        raise SystemExit("no rows produced; nothing ran")

    print(f"rows written to {out_dir}", file=sys.stderr)
    return subprocess.run(
        [sys.executable, str(REPO / "scripts" / "compare_benchmarks.py"), *[str(path) for path in written]],
        check=False,
    ).returncode


if __name__ == "__main__":
    sys.exit(main())
