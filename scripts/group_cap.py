#!/usr/bin/env python3
"""How many plan tasks should run at once? Measure the task, do not count cores.

ADR 0032 first said "cap a compile-heavy group at about half the cores", then measured that advice
and found it was a rule about *threads* wearing the costume of a rule about *tasks*. The two shapes
disagree by 2x:

  8 single-threaded crate builds, 6 cores     cap 3: 15 s   cap 6: 10 s   uncapped: 8 s
  3 workspace builds, each `cargo -j6`        cap 3: 43 s   (the lock serialises; isolation 46 s)

A task that uses one core wants every core filled with tasks. A task that uses six cores wants to be
alone. So the cap is `nproc / threads-per-task`, and threads-per-task is measurable: run the task
once under `time` and divide CPU seconds by wall seconds.

    python3 scripts/group_cap.py --measure "cargo check --locked --all-features"
    python3 scripts/group_cap.py --threads 1 --evidence

It reports a recommendation and, with --evidence, the measurements the recommendation came from. It
does not project wall clock: a model with a made-up contention term is worse than a measured table.
"""

from __future__ import annotations

import argparse
import os
import resource
import subprocess
import sys
import time


def cpu_seconds(command: str) -> tuple[float, float] | None:
    """(wall, cpu) for one run of `command`, summed over its whole process tree.

    `resource.RUSAGE_CHILDREN` accumulates the CPU of terminated descendants, so this needs no
    external `time` binary - which a slim container image does not have.
    """
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    try:
        subprocess.run(["sh", "-c", command], capture_output=True, check=False)
    except OSError:
        return None
    wall = time.monotonic() - started
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    return wall, cpu


def recommend(threads: float, cores: int) -> int:
    """Tasks that fit in the cores without oversubscribing, and never more than the cores."""
    usable = max(1.0, threads)
    return max(1, min(cores, int(cores // usable)))

EVIDENCE = """\
Measured on six cores (ADR 0032, both amendments). A longer bar is a slower group.

  eight single-threaded crate builds            one crate alone: 5 s
    sequential           24 s   ##############################   100%
    capped at 3 (half)   15 s   ###################...........    62%
    capped at 6 (cores)  10 s   ############..................    42%
    uncapped, 8 at once   8 s   ##########....................    33%

  twelve single-threaded crate builds           plan group 1 is 11
    sequential           24 s   ##############################   100%
    capped at 3 (half)   15 s   ###################...........    62%
    capped at 6 (cores)   9 s   ###########...................    38%
    uncapped, 12 at once  8 s   ##########....................    33%

  three workspace builds, each `cargo -j6`      45 s each alone
    sequential           45 s   ##############################   100%
    concurrent           43 s   #############################.    96%
    concurrent, isolated 46 s   ##############################   102%

The first two shapes are hurt by a cap; the third is not helped by one. Threads per task is the
variable, which is why this script measures it instead of counting tasks. Isolation loses in the
third shape because three cold dependency trees oversubscribe six cores.
"""


def self_test() -> int:
    """The recommendation has to agree with every shape ADR 0032 measured."""
    failures = []

    def check(condition: bool, message: str) -> None:
        if not condition:
            failures.append(message)

    # One thread per task on six cores: fill the machine, never cap at half.
    check(recommend(1.0, 6) == 6, f"one-thread task on 6 cores gave {recommend(1.0, 6)}, wanted 6")
    # A task that already fills the machine runs alone.
    check(recommend(6.0, 6) == 1, f"six-thread task on 6 cores gave {recommend(6.0, 6)}, wanted 1")
    check(recommend(4.0, 8) == 2, f"four-thread task on 8 cores gave {recommend(4.0, 8)}, wanted 2")
    # Never exceed the cores, even for a task cheaper than one core (a warm cache measures < 1).
    check(recommend(0.4, 6) == 6, f"sub-core task gave {recommend(0.4, 6)}, wanted 6 (the core count)")
    check(recommend(0.0, 4) == 4, f"zero-thread task gave {recommend(0.0, 4)}, wanted 4")
    # Single core machines still admit one task.
    check(recommend(5.0, 1) == 1, f"1-core machine gave {recommend(5.0, 1)}, wanted 1")

    # Every evidence row must carry a wall time and a percentage, or the table is decoration.
    rows = [line for line in EVIDENCE.splitlines() if line.strip().startswith((
        "sequential", "capped", "uncapped", "concurrent"))]
    check(len(rows) == 11, f"expected 11 evidence rows, found {len(rows)}")
    for row in rows:
        check(row.rstrip().endswith("%"), f"row has no percentage: {row!r}")
    for shape in ("eight single-threaded", "twelve single-threaded", "three workspace builds"):
        check(shape in EVIDENCE, f"evidence is missing the {shape!r} shape")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    if failures:
        return 1
    print(f"self-test: {len(rows)} evidence rows and 6 recommendation cases agree with ADR 0032")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--measure", metavar="CMD", help="time this command once to infer threads")
    parser.add_argument("--threads", type=float, help="threads one task uses, if already known")
    parser.add_argument("--evidence", action="store_true", help="print the measured tables")
    parser.add_argument("--self-test", action="store_true", help="check the recommendation and tables")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    cores = os.cpu_count() or 1
    threads = args.threads

    if args.measure:
        measured = cpu_seconds(args.measure)
        if measured is None:
            print("could not time that command; pass --threads instead", file=sys.stderr)
            return 1
        wall, cpu = measured
        threads = cpu / wall if wall > 0 else 1.0
        print(f"measured: wall {wall:.1f}s, cpu {cpu:.1f}s -> {threads:.1f} threads per task")

    if threads is None:
        if args.evidence:
            print(EVIDENCE.strip())
            return 0
        parser.error("give --measure, --threads or --evidence")

    cap = recommend(threads, cores)
    print(f"cores {cores}, threads per task {threads:.1f} -> cap {cap} task(s) at a time")
    if threads >= cores:
        print("  this task already fills the machine; run it alone and let groups of this shape be")
        print("  serialised rather than admitting more work than the cores can hold")
    elif threads <= 1.5:
        print("  this task uses about one core; fill the machine with tasks, do not cap at half")
    else:
        print("  this task uses several cores; oversubscribing it costs more than it overlaps")

    if args.evidence:
        print(EVIDENCE.strip())
    return 0


if __name__ == "__main__":
    sys.exit(main())
