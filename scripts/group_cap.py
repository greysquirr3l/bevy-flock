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
import pathlib
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


def derive_groups(tasks: dict[str, list[str]]) -> list[list[str]]:
    """The execution groups wiggum derives from `depends_on`: a task joins the first wave after its
    slowest dependency. Unknown dependencies (already-completed tasks) are ignored. Raises ValueError
    on a cycle, because a cycle has no schedule and silently dropping it would hide a broken plan."""
    wave: dict[str, int] = {}

    def place(task: str, chain: tuple[str, ...] = ()) -> int:
        if task in wave:
            return wave[task]
        if task in chain:
            cycle = " -> ".join((*chain, task))
            raise ValueError(f"dependency cycle: {cycle}")
        ready = [place(dep, (*chain, task)) for dep in tasks[task] if dep in tasks]
        wave[task] = max(ready, default=0) + 1
        return wave[task]

    for task in tasks:
        place(task)
    groups: list[list[str]] = [[] for _ in range(max(wave.values(), default=0))]
    for task, depth in wave.items():
        groups[depth - 1].append(task)
    return [sorted(group) for group in groups if group]


def load_plan(path: str) -> tuple[dict[str, list[str]], list[str]]:
    """Live tasks (queue) and completed task names from a wiggum plan.toml."""
    import tomllib

    doc = tomllib.loads(pathlib.Path(path).read_text())
    done = {entry["slug"] for entry in doc.get("completed", [])}
    tasks = {
        task["slug"]: list(task.get("depends_on", []))
        for phase in doc.get("phases", [])
        for task in phase.get("tasks", [])
        if task["slug"] not in done
    }
    return tasks, sorted(done)


def parse_validate(text: str) -> list[list[str]]:
    """Read `wiggum validate`'s group listing.

    The authored plan cannot reproduce wiggum's groups exactly: wiggum appends up to two audit tasks
    (`integration-wiring`, `stub-cleanup`) that no phase declares, and later tasks depend on them. So
    when wiggum is installed, its own answer is the one to size.
    """
    groups = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped.startswith("Group ") or ":" not in stripped:
            continue
        names = stripped.split(":", 1)[1].replace("(concurrent)", "")
        members = [name.strip() for name in names.split(",") if name.strip()]
        if members:
            groups.append(members)
    return groups


def check_plan(path: str, threads: float, cores: int) -> tuple[list[list[str]], int, int]:
    """Groups, the cap they should be admitted under, and how many groups exceed it."""
    tasks, _done = load_plan(path)
    groups = derive_groups(tasks)
    cap = recommend(threads, cores)
    over = sum(1 for group in groups if len(group) > cap)
    return groups, cap, over


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

    # Group derivation must reproduce wiggum's waves, not a plausible approximation of them.
    fixture = {"a": [], "b": [], "c": ["a"], "d": ["a", "b"], "e": ["c", "d"]}
    check(derive_groups(fixture) == [["a", "b"], ["c", "d"], ["e"]],
          f"wave derivation gave {derive_groups(fixture)}")
    # A dependency on an already-completed task cannot hold a group open.
    check(derive_groups({"a": ["finished"], "b": []}) == [["a", "b"]],
          "a completed dependency created a phantom wave")
    try:
        derive_groups({"a": ["b"], "b": ["a"]})
        failures.append("a dependency cycle was scheduled instead of reported")
    except ValueError:
        pass
    # `wiggum validate` output: sizes come from its groups, not from a plausible-looking regex.
    validate_text = """  Plan: bevy-flock
     Group 1: alpha, beta, gamma (concurrent)
     Group 2: delta (concurrent)
"""
    check(parse_validate(validate_text) == [["alpha", "beta", "gamma"], ["delta"]],
          f"validate parsing gave {parse_validate(validate_text)}")
    check(parse_validate("") == [], "empty validate output produced groups")

    # The real plan must parse and derive, whatever its groups turn out to be.
    plan = pathlib.Path(__file__).resolve().parent.parent / "plan.toml"
    if plan.exists():
        groups, cap, _over = check_plan(str(plan), 1.0, 6)
        check(len(groups) > 0, "the repository plan produced no groups")
        check(all(groups), "a group came out empty")
    else:
        failures.append(f"plan not found at {plan}")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    if failures:
        return 1
    print(
        f"self-test: {len(rows)} evidence rows, every recommendation and derivation case "
        f"agrees with ADR 0032 and ADR 0033"
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--measure", metavar="CMD", help="time this command once to infer threads")
    parser.add_argument("--threads", type=float, help="threads one task uses, if already known")
    parser.add_argument("--evidence", action="store_true", help="print the measured tables")
    parser.add_argument("--self-test", action="store_true", help="check the recommendation and tables")
    parser.add_argument("--check-plan", metavar="PATH", help="derive the plan's groups and size them")
    parser.add_argument("--groups-from", metavar="PATH", help="use `wiggum validate` output, which is exact")
    parser.add_argument("--strict", action="store_true", help="with --check-plan, exit 1 when a group exceeds the cap")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    if args.check_plan or args.groups_from:
        cores = os.cpu_count() or 1
        threads = args.threads if args.threads is not None else float(cores)
        if args.groups_from:
            groups = parse_validate(pathlib.Path(args.groups_from).read_text())
            cap = recommend(threads, cores)
            over = sum(1 for group in groups if len(group) > cap)
            source = f"{args.groups_from} (wiggum's groups, exact)"
        else:
            groups, cap, over = check_plan(args.check_plan, threads, cores)
            source = f"{args.check_plan} (authored tasks only; wiggum adds up to two audit tasks)"
        print(f"{source}: {sum(len(g) for g in groups)} tasks, {len(groups)} groups")
        print(f"cores {cores}, threads per task {threads:.1f} -> admit {cap} at a time")
        print()
        for index, group in enumerate(groups, start=1):
            mark = "OVER" if len(group) > cap else "ok"
            print(f"  group {index}: {len(group):>2} tasks  {mark}")
        print()
        print("Groups encode ordering, not capacity. Taking the first `cap` tasks of a group and")
        print("re-deriving is the supported way to run a group larger than the machine.")
        if over and args.strict:
            print(f"\nFAIL {over} group(s) exceed the cap of {cap}", file=sys.stderr)
            return 1
        return 0

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
