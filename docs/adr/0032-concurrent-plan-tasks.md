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

## What this does not answer

Whether a group of *five* tasks contending for cores is faster than the same five in sequence. The
two-task group is dominated by the build lock, and a benchmark that shares a machine with three
compilations measures the machine, not the code — which is why benchmark-bearing tasks belong in a
group of their own, the same way CI gives them their own runner.
