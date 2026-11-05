# ADR 0032 — Concurrent plan tasks: what the build pipeline tolerates

**Status:** Accepted (2026-11-04)
**Context:** wiggum schedules `cell-size-reopen-gate` and `examples-showcase` in the same execution
group. Both were run concurrently, in one worktree, to find out what that actually costs.

## The run

Two tasks, one worktree, started together:

| task | work | result |
| --- | --- | --- |
| `examples-showcase` | `cargo build --examples --all-features`, then `hello_flock` | build 0, run 0 |
| `cell-size-reopen-gate` | `cargo bench -- flock_frame_cell`, then `scripts/cell_size_gate.py` | bench 0, gate PASS (0.1% saving, decision stands) |

**36.3 s wall, both green.** Nothing failed, nothing corrupted. That is the headline, and it is not
the interesting part.

## What is safe, measured

**Mixed feature sets do not thrash the cache.** The showcase builds `--all-features`; the gate builds
the default set. Measured on this tree: all-features 29 s, default immediately after 3 s, default
again 1 s, back to all-features 0 s. Cargo keeps per-unit fingerprints and unifies dependency
features, so the two tasks recompile this crate's own units (seconds) rather than the dependency
tree (minutes). A concurrent group does not need matching feature sets.

**The build lock serialises compile phases, which is what it is for.** Two `cargo` invocations in one
target directory queue their builds and then run their benchmarks at full speed. The cost is
latency — a long build delays the other task's build — not correctness. The pipeline tolerates it.

## What is not safe, measured

**Two tasks cannot stage in the same worktree.** Forty files, three rounds, four parallel `git add`
processes: **76 of 120 invocations failed** with `Unable to create index.lock: File exists`. Git's
index is a single file guarded by a lock file; concurrent writers collide roughly two times in three,
and the failure is silent in a script that does not check exit codes — the work is on disk but
missing from the commit.

That is the only hard rule this experiment produced.

## The decision

1. **A concurrent group partitions by file ownership.** Two tasks may not write the same file, and
   `examples-showcase` and `cell-size-reopen-gate` are a well-chosen pair because one owns `examples/`
   and `README`, the other owns `scripts/`, `docs/adr/0031` and CI.
2. **Exactly one task in a group commits.** Everything else leaves the tree dirty and says so;
   staging and committing run once, after the group finishes. A group with two committers is a group
   that silently loses work.
3. **CI needs no rules of its own.** Every job gets its own runner and cache; the contention measured
   here is a property of an orchestrator sharing one worktree, which CI does not do.
4. **Isolation remains available where it matters.** `CARGO_TARGET_DIR` per task, or a worktree per
   task, removes both the build-lock queue and the index hazard outright. This ADR does not require
   it — 36 s of a group is not worth the setup — but a group whose tasks both build from cold should
   use it, because two cold builds serialise inside one target directory and overlap inside two.

## Amendment: three tasks, measured (same day)

Decision 4 above said a group building from cold "should use isolation, because two cold builds
serialise inside one target directory and overlap inside two". Three tasks on a six-core machine say
that is wrong, and this is the correction.

Three genuinely different compile tasks — `--all-features`, `--no-default-features`,
`--features memory,serde` — each configuration starting from a cold target directory so the numbers
measure compilation rather than cache warm-up:

| configuration | wall | vs baseline | efficiency (3 tasks) |
| --- | --- | --- | --- |
| one target, sequential | 45 s | 100% | 33% |
| one target, concurrent | 43 s | 105% | 35% |
| three targets, concurrent | 46 s | 98% | 33% |
| three targets, sequential | 82 s | 55% | 18% |

**The lock is nearly free (45 s → 43 s, +4%).** ADR 0032 called the build lock "latency, not
correctness" and implied a real cost. There is almost none: a serialised `cargo` build still uses
every core, so queueing three CPU-bound builds costs what running them on a contended machine costs
anyway.

**Isolation does not pay at this size (46 s vs 43 s).** Three cold target directories rebuild the
dependency tree three times; on six cores, three `cargo` invocations each defaulting to `-j6`
oversubscribe eighteen compiler threads onto six and the overlap is eaten by contention. Isolation
bought a 44% win against *sequential isolated* work (82 s → 46 s), which is the wrong comparison —
against the shared cache it bought nothing.

**Feature subsets come free from a shared cache.** The third task (`memory,serde`, a subset of
`all-features`) compiled in 0 s in the shared directory. That is the mechanism behind the two-task
result: a shared target directory is not just a cache, it is a cache of *overlapping* work, and a
group whose tasks share a feature lineage is cheaper together than apart.

### What replaces decision 4

- **Keep the shared target directory as the default**, even cold. Isolation is not a performance
  win on a six-core box; it is a diagnostic tool for a corrupted cache and a requirement only when
  tasks must not share feature unification at all (for example a `--no-default-features` build that
  must prove it does not link an optional dependency).
- **Cap a compile-heavy group at about half the cores.** Three tasks on six cores is at the edge;
  the plan's group 1 schedules *eleven*, which on this machine would spend its wall time in the
  scheduler. The plan's grouping is dependency-derived and says nothing about capacity — that is a
  gap, and it is now a known one rather than an assumed non-issue.
- **Benchmark-bearing tasks still get their own group.** That part of the original decision stands
  and is strengthened: with three compilations running, the machine cannot measure 3% of a frame.

## What this still does not answer

Whether a group of *eleven* tasks — the size the plan actually schedules — is faster than eleven in
sequence. Three tasks on six cores already show efficiency falling to 33% and isolation failing to
recover it; the useful experiment at that size is a scheduler that admits work by CPU count rather
than a dependency graph alone, and that is a change to the orchestration, not to this repository.
