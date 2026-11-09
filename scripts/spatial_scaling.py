#!/usr/bin/env python3
"""Plot the runtime thread-scaling sweep from `examples/scaling_sweep.rs`.

Two panels, both built only from measurements:

  left    milliseconds per phase against worker count, one line per phase, with the serial cost of each
          phase drawn as a dashed horizontal line so the gap a new thread count has to close is visible
  right   speed-up against the single-thread run, with the machine's core count marked, because every
          point past it is oversubscription and the plan's question was where the ceiling is

The figure is not decoration: the table printed to stdout is the same data, and `docs/performance.md`
quotes that table rather than the picture. A figure that disagreed with its own table would be a bug
here, not a rounding difference.

    cargo run --release --example scaling_sweep > /tmp/spatial-scaling.json
    python3 scripts/spatial_scaling.py /tmp/spatial-scaling.json docs/images/spatial-scaling.png
"""
from __future__ import annotations

import json
import pathlib
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402  (backend must be chosen before pyplot)

PHASES = ("build", "batch", "frame")
COLOURS = {"build": "#1f77b4", "batch": "#2ca02c", "frame": "#d62728"}


def load(path: pathlib.Path) -> tuple[dict, dict[tuple[str, int], float]]:
    meta: dict = {}
    cells: dict[tuple[str, int], float] = {}
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        record = json.loads(line)
        if record["kind"] == "meta":
            meta = record
        else:
            cells[(record["phase"], int(record["threads"]))] = float(record["seconds"])
    if not meta or not cells:
        raise SystemExit(f"{path}: no meta record or no cells")
    return meta, cells


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    source = pathlib.Path(sys.argv[1])
    target = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else None
    meta, cells = load(source)
    threads = sorted({thread for (_, thread) in cells})
    cores = int(meta["cores"])

    print(f"# machine: {meta['machine']}, {cores} cores; bodies {meta['bodies']}, "
          f"queries {meta['queries']}, repeats {meta['repeats']}")
    print("| Phase | " + " | ".join(f"{thread} thread" + ("s" if thread > 1 else "") for thread in threads) + " |")
    print("| --- | " + " | ".join("---:" for _ in threads) + " |")
    for phase in PHASES:
        row = [f"{cells[(phase, thread)] * 1e3:.2f} ms" for thread in threads]
        print(f"| {phase} | " + " | ".join(row) + " |")
    print("| Phase | " + " | ".join(f"{thread}t" for thread in threads) + " |")
    print("| --- | " + " | ".join("---:" for _ in threads) + " |")
    for phase in PHASES:
        base = cells[(phase, 1)]
        row = [f"{base / cells[(phase, thread)]:.2f}×" for thread in threads]
        print(f"| {phase} speed-up | " + " | ".join(row) + " |")

    figure, (left, right) = plt.subplots(1, 2, figsize=(11.5, 4.4), dpi=150)
    for phase in PHASES:
        series = [cells[(phase, thread)] * 1e3 for thread in threads]
        left.plot(threads, series, marker="o", color=COLOURS[phase], label=phase)
        left.annotate(
            f"{series[0]:.1f}",
            (threads[0], series[0]),
            textcoords="offset points",
            xytext=(6, 6),
            fontsize=8,
            color=COLOURS[phase],
        )
        right.plot(
            threads,
            [cells[(phase, 1)] / cells[(phase, thread)] for thread in threads],
            marker="o",
            color=COLOURS[phase],
            label=phase,
        )
    right.plot(threads, threads, linestyle=":", color="#666666", label="perfect scaling")
    for panel in (left, right):
        panel.axvline(cores, linestyle="--", color="#999999", linewidth=1)
        panel.set_xticks(threads)
        panel.set_xlabel("workers (thread::available_parallelism = %d)" % cores)
        panel.grid(alpha=0.25)
        panel.legend(frameon=False, fontsize=8)
    left.set_ylabel("milliseconds per tick")
    left.set_title("Cost per phase, 1M bodies, 5k queries", fontsize=10)
    right.set_ylabel("speed-up against one worker")
    right.set_title("Where the ceiling is: 6 cores, then oversubscription", fontsize=10)
    figure.suptitle(
        "bevy-flock spatial index, runtime thread scaling (measured, median of %d runs)" % meta["repeats"],
        fontsize=10,
    )
    figure.tight_layout()
    if target is not None:
        target.parent.mkdir(parents=True, exist_ok=True)
        figure.savefig(target)
        print(f"wrote {target}")


if __name__ == "__main__":
    main()
