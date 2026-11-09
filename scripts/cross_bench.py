#!/usr/bin/env python3
"""Run the cross-engine fixture on this machine and print the table.

    python3 scripts/cross_bench.py --agents 10000,100000
    python3 scripts/cross_bench.py --agents 10000 --engines bevy-flock,godot-grid --out /tmp/bench
    python3 scripts/cross_bench.py --engines unreal \
        --unreal-cmd 'UnrealEditor-Cmd ~/CrossBench/CrossBench.uproject -game -nullrhi \
                      -crossbenchagents={agents} -crossbenchticks={ticks} -crossbenchwarmup={warmup} -log'

Item 7 of the 1.0 checklist is "comparable performance, on one machine", and the hard part was never the measuring:
it was remembering six commands, two of which belong to a different engine, and getting the same agent count and tick
count into both. This script is that memory. It runs each harness with identical arguments, writes one JSON line per
row, then hands the files to `compare_benchmarks.py`, whose refusal to compare across machines is the reason the runner
never merges results from different runs.

Engines it knows about:

  bevy-flock   examples/cross_bench.rs, release build, `App::update` per tick
  godot-array  the GDScript fixture with no spatial index, a lower bound for Godot
  godot-grid   the same fixture plus a uniform-grid rebuild, the like-for-like row
  unreal       benchmarks/unreal, driven through --unreal-cmd because a UE project is not built here

A missing engine is skipped with a note rather than a failure, so a machine without Godot can still measure our side.
The exception is `unreal`: it needs a command template, and asking for it without one is a mistake worth stopping for.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
GODOT_DIR = REPO / "benchmarks" / "godot"
ENGINES = ("bevy-flock", "godot-array", "godot-grid", "unreal")
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


def run(command: list[str] | str, cwd: pathlib.Path | None = None, shell: bool = False) -> str:
    """Run a harness and return its stdout, refusing to hide a non-zero exit."""
    completed = subprocess.run(command, cwd=cwd, capture_output=True, text=True, check=False, shell=shell)
    if completed.returncode != 0:
        shown = command if isinstance(command, str) else " ".join(command)
        raise SystemExit(f"command failed ({completed.returncode}): {shown}\n{completed.stderr.strip()[:2000]}")
    return completed.stdout


def payloads(output: str) -> list[str]:
    """Keep the JSON objects, whatever prefix the engine's log put in front of them."""
    rows = []
    for line in output.splitlines():
        marker = line.find("crossbench {")
        if marker >= 0:
            rows.append(line[marker + len("crossbench ") :].strip())
    return rows


def bevy_rows(agents: list[int], ticks: int, warmup: int) -> list[str]:
    example = REPO / "target" / "release" / "examples" / "cross_bench"
    if not example.exists():
        raise SystemExit(f"{example} is missing: build it first with\n  cargo build --release --example cross_bench")
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
        rows.extend(payloads(output))
    return rows


def unreal_rows(agents: list[int], ticks: int, warmup: int, template: str) -> list[str]:
    """Run a user-supplied Unreal command once per agent count.

    The template is a shell command with {agents}, {ticks} and {warmup} placeholders, because the path to an engine
    install is not something this repository can guess and a half-configured harness is worse than none.
    """
    rows = []
    for count in agents:
        command = template.format(agents=count, ticks=ticks, warmup=warmup)
        output = run(command, shell=True)
        found = payloads(output)
        if not found:
            raise SystemExit(
                f"the Unreal harness printed no crossbench row for {count} agents; run the command by hand to see why:\n"
                f"  {command}"
            )
        rows.extend(found)
    return rows


def self_test() -> int:
    """Prove the Unreal path end to end without an engine: a fake command that prints a well-formed row."""
    import tempfile

    with tempfile.TemporaryDirectory() as directory:
        fake = pathlib.Path(directory) / "fake_unreal.py"
        fake.write_text(
            "import sys\n"
            "agents, ticks, warmup = sys.argv[1], sys.argv[2], sys.argv[3]\n"
            'print("LogTemp: crossbench {\\"engine\\":\\"unreal\\",\\"agents\\":" + agents + ",\\"ticks\\":" + ticks + '
            '",\\"warmup\\":" + warmup + ",\\"median_ms\\":1.0,\\"p95_ms\\":1.1,\\"p99_ms\\":1.2,\\"max_ms\\":1.3,'
            '\\"over_budget_pct\\":0.0,\\"budget_ms\\":16.6,\\"machine\\":\\"unknown cpu, 6 threads\\"}")\n'
        )
        template = f"{sys.executable} {fake} {{agents}} {{ticks}} {{warmup}}"
        rows = unreal_rows([500, 1500], 100, 10, template)
        if len(rows) != 2:
            print(f"self-test: expected 2 rows, got {len(rows)}", file=sys.stderr)
            return 1
        parsed = [json.loads(row) for row in rows]
        if [row["agents"] for row in parsed] != [500, 1500]:
            print("self-test: the agent counts were not substituted", file=sys.stderr)
            return 1
        if parsed[0]["ticks"] != 100 or parsed[0]["warmup"] != 10:
            print("self-test: the tick counts were not substituted", file=sys.stderr)
            return 1
        missing = [key for key in REQUIRED if key not in parsed[0]]
        if missing:
            print(f"self-test: the row is missing {', '.join(missing)}", file=sys.stderr)
            return 1
        # A row that the prefix-tolerant parser cannot see is a row the table silently drops, and an engine log puts
        # a prefix on every line it prints.
        prefixed = "LogCrossBench: Display: crossbench " + rows[0]
        if payloads(prefixed) != [rows[0]]:
            print("self-test: a prefixed row was not recovered", file=sys.stderr)
            return 1
    print("cross_bench self-test: the Unreal path substitutes, parses and validates")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--agents", default="10000,100000", help="comma-separated agent counts")
    parser.add_argument("--ticks", type=int, default=1000, help="measured ticks per row")
    parser.add_argument("--warmup", type=int, default=100, help="unmeasured ticks before the measured ones")
    parser.add_argument("--engines", default=",".join(ENGINES), help=f"any of {', '.join(ENGINES)}")
    parser.add_argument("--out", default="", help="directory for the JSONL files (default: target/cross-bench)")
    parser.add_argument(
        "--unreal-cmd",
        default="",
        help="shell command for the Unreal harness, with {agents}, {ticks} and {warmup} placeholders",
    )
    parser.add_argument("--self-test", action="store_true", help="exercise the Unreal path without an engine")
    arguments = parser.parse_args()

    if arguments.self_test:
        return self_test()

    counts = [int(value) for value in arguments.agents.split(",") if value.strip()]
    wanted = [name.strip() for name in arguments.engines.split(",") if name.strip()]
    unknown = [name for name in wanted if name not in ENGINES]
    if unknown:
        raise SystemExit(f"unknown engine(s): {', '.join(unknown)}; known: {', '.join(ENGINES)}")
    if "unreal" in wanted and not arguments.unreal_cmd:
        raise SystemExit(
            "unreal needs --unreal-cmd, because a UE project is not built by this repository. Example:\n"
            "  --unreal-cmd 'UnrealEditor-Cmd ~/CrossBench/CrossBench.uproject -game -nullrhi "
            "-crossbenchagents={agents} -crossbenchticks={ticks} -crossbenchwarmup={warmup} -log'"
        )

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

    if "unreal" in wanted:
        path = out_dir / "unreal.jsonl"
        rows = unreal_rows(counts, arguments.ticks, arguments.warmup, arguments.unreal_cmd)
        path.write_text("\n".join(rows) + "\n")
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
