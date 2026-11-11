#!/usr/bin/env python3
"""Count the machine's physical cores and pick one logical CPU per core.

The scaling sweep pins CPU sets, and a pin of two hardware threads of one core is not two cores:
two threads of one core deliver roughly 1.3x the throughput of one thread, so a row measured on
them says something different from a row measured on two real cores, and the two are not comparable.
Linux writes each CPU's siblings in sysfs, and that is where the cores come from. Where sysfs does
not say (a kernel without topology, a hypervisor that hides it), one logical CPU per core is the
only assumption available, and the caller records that it was assumed rather than measured.

    python3 scripts/cpu_topology.py           # prints "<core count> <cpu ids> <measured|assumed>"
    python3 scripts/cpu_topology.py --self-test
"""

from __future__ import annotations

import argparse
import glob
import os
import pathlib
import re
import sys
import tempfile

DEFAULT_SYSFS_ROOT = "/sys/devices/system/cpu"
DEFAULT_STATUS = "/proc/self/status"


def parse_cpu_list(text: str) -> set[int]:
    """`0-3,8` as a set of CPU ids."""
    ids: set[int] = set()
    for part in text.strip().split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            low, high = part.split("-")
            ids.update(range(int(low), int(high) + 1))
        else:
            ids.add(int(part))
    return ids


def allowed_cpus(status_path: str = DEFAULT_STATUS) -> set[int]:
    """The CPUs this process may run on, which is what a pin has to stay inside."""
    fallback = set(range(os.cpu_count() or 1))
    try:
        with open(status_path) as handle:
            for line in handle:
                if line.startswith("Cpus_allowed_list:"):
                    return parse_cpu_list(line.split(":", 1)[1])
    except OSError:
        pass
    return fallback


def detect(
    sysfs_root: str = DEFAULT_SYSFS_ROOT,
    allowed: set[int] | None = None,
) -> tuple[list[int], bool]:
    """One logical CPU per physical core, and whether that came from the machine or was assumed.

    The representative is the lowest allowed CPU of each core, so the same machine always pins the
    same CPUs and two runs on it are comparable.
    """
    usable = allowed_cpus() if allowed is None else allowed
    cores: dict[frozenset[int], int] = {}
    pattern = os.path.join(sysfs_root, "cpu[0-9]*", "topology", "thread_siblings_list")
    for path in sorted(glob.glob(pattern)):
        match = re.search(r"cpu(\d+)", path)
        if match is None:
            continue
        try:
            with open(path) as handle:
                siblings = parse_cpu_list(handle.read())
        except OSError:
            continue
        in_core = siblings & usable
        if in_core:
            cores[frozenset(siblings)] = min(in_core)
    if not cores:
        # No topology to read: one logical CPU per core, which is right on a machine without SMT.
        return sorted(usable), False
    return sorted(cores.values()), True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--sysfs-root", default=DEFAULT_SYSFS_ROOT, help="where to read the CPU topology")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    reps, measured = detect(args.sysfs_root)
    if not reps:
        print("no usable CPUs", file=sys.stderr)
        return 1
    # One line, three fields: the count, the CPU ids as a comma-separated list taskset accepts, and
    # whether the count came from the machine or was assumed.
    print(len(reps), ",".join(str(rep) for rep in reps), "measured" if measured else "assumed")
    return 0


def self_test() -> int:
    failures: list[str] = []

    def tree(root: pathlib.Path, siblings: dict[int, str]) -> None:
        for cpu, list_text in siblings.items():
            path = root / f"cpu{cpu}" / "topology"
            path.mkdir(parents=True, exist_ok=True)
            (path / "thread_siblings_list").write_text(list_text + "\n")

    with tempfile.TemporaryDirectory() as raw:
        root = pathlib.Path(raw)

        # 1. Two cores with two threads each: one CPU per core, not one per thread.
        paired = root / "paired"
        tree(paired, {0: "0,1", 1: "0,1", 2: "2,3", 3: "2,3"})
        reps, measured = detect(str(paired), allowed={0, 1, 2, 3})
        if reps != [0, 2]:
            failures.append(f"paired cores were not grouped into two: {reps}")
        if not measured:
            failures.append("a tree with sibling lists was reported as assumed")

        # 2. A machine without SMT: every CPU is its own core.
        plain = root / "plain"
        tree(plain, {0: "0", 1: "1", 2: "2"})
        reps, measured = detect(str(plain), allowed={0, 1, 2})
        if reps != [0, 1, 2] or not measured:
            failures.append(f"a machine without SMT was misread: {reps}")

        # 3. A cpuset that excludes half the machine: the pin stays inside what it may use.
        restricted = root / "restricted"
        tree(restricted, {0: "0,1", 1: "0,1", 2: "2,3", 3: "2,3"})
        reps, _ = detect(str(restricted), allowed={2, 3})
        if reps != [2]:
            failures.append(f"a restricted cpuset was not respected: {reps}")

        # 4. No topology at all: one CPU per core, and the caller is told it was assumed.
        empty = root / "empty"
        empty.mkdir()
        reps, measured = detect(str(empty), allowed={0, 1})
        if reps != [0, 1]:
            failures.append(f"the fallback did not use every allowed CPU: {reps}")
        if measured:
            failures.append("a missing topology was reported as measured")

        # 5. CPU lists in the shapes the kernel writes them.
        if parse_cpu_list("0-3,8\n") != {0, 1, 2, 3, 8}:
            failures.append("a CPU list with a range and a single id was misparsed")
        if parse_cpu_list("2\n") != {2}:
            failures.append("a single CPU id was misparsed")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    if failures:
        return 1
    print("self-test: cores are counted from the machine's own topology, and a missing one is assumed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
