# ADR 0033, The group capping policy, and what CI validates about it

**Status:** Accepted (2026-11-05). Supersedes the policy sections of ADR 0032; the measurements stay
there.
**Context:** ADR 0032 and its two amendments produced a rule in three pieces: the build lock is cheap,
isolation does not pay, and the cap is a thread budget rather than a task count. Three pieces is a lab
notebook. A policy has to be stated once, in a form someone can follow without reading the history,
and it has to say which parts a machine checks.

## 1. The admission rule

```
cap = clamp(1, nproc, floor(nproc / threads_per_task))
```

Take `cap` tasks from a group at a time. `threads_per_task` is **measured, not assumed**, one run of
the task through `scripts/group_cap.py --measure "<command>"`, which divides total CPU by wall clock.
The two shapes that matter differ by 2× in the same group size:

| shape | one task alone | cap on 6 cores | eight tasks, sequential → uncapped |
| --- | --- | --- | --- |
| one crate's codegen | 5 s | 6 | 24 s → 8 s |
| workspace build, `cargo -j6` | 45 s | 1 | 45 s → 43 s |

A single-crate task leaves five cores idle if you cap it; a workspace build oversubscribes the machine
if you do not. Both numbers are measured in ADR 0032 and printed by `--evidence`.

### Validation across core counts (2026-11-05)

The rule was measured on six cores only when it was written, so it was re-measured with the process
pinned to 1, 2, 3, 4 and 6 cores (`scripts/scaling_sweep.sh`; the numbers live in
`docs/dev/tuning/data/scaling-sweep.json`, the review in `docs/dev/tuning/reports/scaling-report.md`).

- **The formula is a floor with headroom, not an optimum.** On six cores, one-thread tasks: cap 6
  gives 4.1 s and admitting all 8 gives 3.0 s (5.2x against 3.8x), because the resolve/read/spawn
  phase of each cargo invocation overlaps across tasks. On two cores the ordering reverses (1.88x at
  two at once, 1.79x at three), so this is bursty work, not a licence to oversubscribe.
- **Threads per task dominates the curve.** A six-thread bundle alone on six cores reaches 5.6x
  against one core; three together reach 5.8x. The threads inside the task already fill the machine,
  which is exactly why the policy measures threads instead of counting tasks.
- **Concurrency past a task's own thread count buys nothing.** A six-thread bundle on three cores
  takes 12.7 s alone, 12.5 s with two and 12.6 s with three; it is queueing for cores internally.

The first version of this measurement was wrong, and the correction is worth recording: shape B built
three "six-crate" bundles from an eight-crate fixture, so four of the eighteen builds did not exist
and the numbers credited parallelism the work never had (a bundle appeared to reach only 4.0x). The
sweep now builds a fixture sized to the bundles, refuses to compare sweeps whose fixtures differ, and
records the fixture in the file itself.

## 2. Group sizes come from wiggum, not from the plan file

`wiggum validate` injects up to two audit tasks (`integration-wiring`, `stub-cleanup`) that no phase
declares, and later tasks depend on them. Measured today: 23 tasks authored, 24 scheduled, and the
difference changes a group's size. Any capacity check that reads only the phases therefore
under-counts, silently, in the direction that makes the machine look sufficient.

So there are two modes, and the exact one is preferred when wiggum is installed:

```bash
wiggum validate plan.toml > /tmp/validate.txt
python3 scripts/group_cap.py --groups-from /tmp/validate.txt --threads 6   # exact
python3 scripts/group_cap.py --check-plan plan.toml --threads 6            # authored only
```

The second form says so in its own output. A tool that quietly disagrees with the scheduler is worse
than no tool.

## 3. Running a group larger than the cap

1. Take the first `cap` tasks of the group and let them finish.
2. Re-derive: a finished task's dependants become eligible, which usually changes the group.
3. Partition by file ownership (ADR 0032), and let exactly one task commit.
4. `CARGO_TARGET_DIR` isolation is for tasks that must not share feature unification, or for a corrupt
   cache, not for speed. Measured: 46 s isolated against 43 s shared.

The plan stays capacity-blind on purpose. `depends_on` is a dependency graph; a schedule is a
different artefact, and a plan that encoded wall-clock capacity would be wrong on every machine but
the one that wrote it.

## 4. What CI enforces

A new **Scripts** job runs `python3 scripts/group_cap.py --self-test` and `py_compile`. The self-test
is the contract, and it fails the build on:

- **A recommendation that contradicts measurement.** `recommend()` must return 6 for a one-thread task
  on six cores, 1 for a six-thread task, 2 for a four-thread task on eight cores, the core count for a
  sub-core measurement, and 1 on a single-core machine. Changing the formula breaks these.
- **An evidence table that lost a row or a percentage.** Eleven rows, all ending in `%`, all three
  measured shapes present. The tables are the justification; a table that quietly loses a row is how
  this policy would rot.
- **A derivation that disagrees with wiggum's wave rule.** A fixed fixture must yield
  `[[a, b], [c, d], [e]]`; a dependency on an already-completed task must not create a phantom wave; a
  cycle must raise rather than be dropped.
- **A plan that no longer parses or derives**, including wiggum's group output.

**Capacity is deliberately not a CI gate.** Runners have two to four cores, so any strict check would
fail there and be disabled within a week; `--strict` exists for a developer machine, where the cores
are the ones being scheduled. What CI can check objectively, that the rule matches its evidence, that
the plan parses, that groups derive, it checks on every push.

## Consequences

- The policy is one page, and the numbers it rests on are printed by the tool rather than trusted.
- Two commands replace a paragraph of advice: measure the task, size the group.
- `scripts/group_cap.py` cannot be edited to fit a new opinion without the self-test objecting, which
  is the point of putting it in CI.
- **Revisit when:** wiggum admits work by CPU count itself (the tool becomes advisory rather than
  load-bearing), or when a plan's live group exceeds roughly fifty tasks and taking a prefix stops
  being a reasonable manual step.

## Alternatives rejected

- **A fixed cap in tasks** ("half the cores"). Measured at 87% worse for one shape and correct for
  another; a constant cannot be both.
- **Encoding the cap in `plan.toml`.** Wrong layer: the plan describes work, the machine describes
  capacity, and they are versioned separately.
- **Automatic `CARGO_TARGET_DIR` isolation.** Measured as no win at three tasks, and it makes every
  target directory cold.
- **Projecting wall clock from a contention model.** Tried during amendment 2: it recommended cap 1 for
  a six-thread task and predicted identical walls for caps 1 and 11. Deleted in favour of the measured
  table.
