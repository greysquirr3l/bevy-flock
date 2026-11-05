#!/usr/bin/env python3
"""Plot the scaling curves from `scripts/scaling_sweep.sh`, and print the table the figure encodes.

Two panels per shape, both built only from measurements:

  top row      wall clock against tasks admitted at once, one line per core count
  bottom row   speedup against the single-core sequential run, one line per concurrency, with
               min(cores, concurrency x threads-per-task) drawn dotted

A speedup above the dashed line is not a rounding error to hide: the single-core sequential baseline
carries one cargo process startup per task, and parallel runs overlap those starts, so the ratio
flatters concurrency. The caption says so rather than clipping the point.

    scripts/scaling_sweep.sh /tmp/scaling.json
    python3 scripts/scaling_curves.py /tmp/scaling.json docs/images/scaling-curves.png
"""

from __future__ import annotations

import json
import pathlib
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402

SHAPE_TITLES = {
    "A": "one crate per task — 1 thread per task",
    "B": "six crates per task — 6 threads per task",
}


def load(path: str) -> list[dict]:
    return [json.loads(line) for line in pathlib.Path(path).read_text().splitlines() if line.strip()]


def series(rows: list[dict], shape: str, cores: int) -> list[tuple[int, int]]:
    return sorted((r["concurrency"], r["wall"]) for r in rows if r["shape"] == shape and r["cores"] == cores)


def efficiency(rows: list[dict], shape: str, cores: int, concurrency: int) -> float | None:
    baseline = series(rows, shape, cores)
    if not baseline:
        return None
    sequential = baseline[0][1]
    wall = dict(baseline).get(concurrency)
    if not wall:
        return None
    ideal = min(cores, concurrency)
    return (sequential / wall) / ideal * 100.0


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    source = sys.argv[1]
    out = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else pathlib.Path("scaling-curves.png")
    rows = load(source)
    if not rows:
        print("no rows", file=sys.stderr)
        return 1

    shapes = sorted({row["shape"] for row in rows})
    fig, axes = plt.subplots(2, len(shapes), figsize=(6.6 * len(shapes), 8.0), squeeze=False)

    for column, shape in enumerate(shapes):
        cores_list = sorted({r["cores"] for r in rows if r["shape"] == shape})
        tasks = next(r["tasks"] for r in rows if r["shape"] == shape)
        top, bottom = axes[0][column], axes[1][column]

        for cores in cores_list:
            points = series(rows, shape, cores)
            xs = [p[0] for p in points]
            ys = [p[1] for p in points]
            top.plot(xs, ys, marker="o", linewidth=1.8, label=f"{cores} core(s)")

        # Speedup against the one-core sequential run, so every line shares one baseline.
        baseline = series(rows, shape, min(cores_list))[0][1]
        threads = next(r["threads"] for r in rows if r["shape"] == shape)
        concurrencies = sorted({r["concurrency"] for r in rows if r["shape"] == shape})
        for concurrency in concurrencies:
            points = [
                (cores, baseline / wall)
                for cores in cores_list
                for c, wall in series(rows, shape, cores)
                if c == concurrency
            ]
            if not points:
                continue
            bottom.plot(*zip(*points), marker="o", linewidth=1.6, label=f"{concurrency} at once")
        if concurrencies:
            # The ideal depends on threads: a six-thread task already uses six cores when admitted
            # alone, so "ideal" is min(cores, admitted x threads), not min(cores, admitted).
            span = list(cores_list)
            for concurrency in {min(concurrencies), max(concurrencies)}:
                bottom.plot(
                    span,
                    [min(c, concurrency * threads) for c in span],
                    linestyle=":",
                    linewidth=1.1,
                    color="#888",
                )

        top.set_title(f"{SHAPE_TITLES.get(shape, shape)}\n{tasks} tasks, cold target per task", fontsize=10)
        top.set_xlabel("tasks admitted at once")
        top.set_ylabel("wall clock (s)")
        top.set_xscale("log", base=2)
        top.set_xticks(sorted({r["concurrency"] for r in rows if r["shape"] == shape}))
        top.get_xaxis().set_major_formatter(matplotlib.ticker.ScalarFormatter())
        top.grid(alpha=0.25)
        top.legend(fontsize=8, title="cores pinned", title_fontsize=8)

        bottom.plot(
            [], [], linestyle=":", color="#888",
            label=f"ideal min(cores, admitted x {threads} thread{'s' if threads > 1 else ''})",
        )
        bottom.set_xlabel("cores the run was pinned to")
        bottom.set_ylabel("speedup against one-core sequential")
        bottom.set_xscale("log", base=2)
        bottom.set_xticks(cores_list)
        bottom.get_xaxis().set_major_formatter(matplotlib.ticker.ScalarFormatter())
        bottom.set_yscale("log", base=2)
        bottom.set_yticks([1, 2, 3, 4, 6, 8])
        bottom.get_yaxis().set_major_formatter(matplotlib.ticker.ScalarFormatter())
        bottom.grid(alpha=0.25, which="both")
        bottom.legend(fontsize=8, title="tasks admitted", title_fontsize=8)

    fig.suptitle("bevy-flock group scaling: measured wall clock against cores and threads per task", fontsize=12)
    fig.text(
        0.5,
        0.005,
        "Above the dotted ideal line means the single-core baseline paid one cargo start per task that "
        "parallel runs overlap — not that a run beat physics.",
        ha="center",
        fontsize=7.5,
        color="#444",
    )
    fig.tight_layout(rect=(0, 0.03, 1, 0.95))
    out.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(out, dpi=150)
    print(f"wrote {out}")

    print()
    print(f"{'shape':<6}{'cores':>6}{'conc':>6}{'wall':>7}{'speedup':>9}{'ideal':>7}{'eff':>7}")
    for shape in shapes:
        cores_list = sorted({r["cores"] for r in rows if r["shape"] == shape})
        for cores in cores_list:
            points = series(rows, shape, cores)
            sequential = points[0][1]
            for concurrency, wall in points:
                ideal = min(cores, concurrency)
                eff = efficiency(rows, shape, cores, concurrency)
                print(
                    f"{shape:<6}{cores:>6}{concurrency:>6}{wall:>6}s{sequential / wall:>8.2f}x"
                    f"{ideal:>6}x{eff:>6.0f}%"
                )
    return 0


if __name__ == "__main__":
    sys.exit(main())
