# ADR 0034, A regression suite for group scaling, and the report it produces

**Status:** Accepted (2026-11-05)
**Context:** ADR 0033 states the capping policy, ADR 0032 and `docs/dev/tuning/performance.md` carry the
measurements behind it. Measurements that nobody re-runs decay: six months from now the baseline file
would still be there, the scheduler could have got quietly worse, and nothing would say so. The repo
already gates one performance decision this way (`scripts/cell_size_gate.py`, 3% warn / 10% fail on a
single-machine benchmark) and that pattern does not transfer here, because the sweep runs on whatever
machine CI gives us and a four-core runner is not a regression.

## Decision

**1. The metric is speedup, not seconds.** For each cell,

```
speedup(cores, concurrency) = wall(cores, 1) / wall(cores, concurrency)
```

is dimensionless, so the same thresholds mean the same thing on a six-core workstation and a
four-core runner. A drop means the machine used its cores less well than before, on any hardware that
measures the same width in real cores (decision 2).

**2. A core count is a count of cores, not of hardware threads.** The sweep pins one logical CPU per
physical core, read from the machine's own topology (`scripts/cpu_topology.py`), and records the cores,
the threads and the pinned CPUs in its meta record. Two threads of one core deliver about 1.3x, not
2x, so a row measured on them is not a row of cores and cannot be compared with a row measured on two
real cores. This is not a nicety. The CI runner is a four-vCPU machine with two physical cores, and
its four-wide row measured 2.29x where the six-core baseline measured 3.73x: the gate read that 38%
loss as a regression, and it was the hardware, not the code. A row of threads is a row about threads,
and the gate names it as one.

The recorded baseline predates the topology record. It pinned logical CPU ids (`taskset -c 0-N-1`) on a
host whose SMT layout was never captured, so its wider rows count hardware threads. Rows 1 and 2 of it
are known to be real cores: the width-2 row reaches 1.878x on two concurrent builds, and one core's two
threads cannot do that. Width 3 and above are ambiguous, so they are named and left out of the comparison
rather than compared and failed. Re-recording the baseline on a host whose topology is captured restores
the wider rows; until then the gate compares widths 1 and 2, and says so in its output.

**3. Noise sets the threshold, and the threshold moves.** Every cell is the median of `REPEATS` runs
(schema 2 records the spread between them). A threshold tighter than the spread is not signal, so each
one widens by the measured spread, a warning needs 1.5x the spread, a failure 2x, because a failure
should be harder evidence than a nag:

```
widest  = max(8% floor, baseline spread, candidate spread)
warn_at = max(12%, 1.5 x widest)
fail_at = max(25%, 2.0 x widest)
```

On the recorded baseline the widest spread is 8.8%, so CI applies 13.2% and 25.0%. A cell between
those lines is a warning that does not block the build: it is the band where a human should look at the
curve but nobody has to re-run CI. A cell past the failure line names its shape, core count,
concurrency and the percentage it lost, so the log says what broke rather than "regression".

Two cells are named rather than painted green. First, a cell whose own spread pushes the failure line past
25% has not been checked, reporting it `ok` would be a tick over an empty test, so it is reported
`unverified`, counted separately, and the fix is more repeats. Second, the sequential cell is the
denominator of every row on its core count, so its spread is folded into those rows: a ragged sequential
run inflates speedups in the flattering direction, and the verification of 2026-11-05 caught exactly that
(one 17.7 s outlier against a 15.8 s baseline turned five `A` rows into apparent gains of 9-12%). If fewer
than half the comparable cells can be verified, the suite exits 2 rather than passing on a coin toss.

The first draft used 35% and 18% with the spread widening only the warning. A dry run killed it: a
deliberately injected 45% wall-clock slowdown on one cell only warned, because 45% more wall clock is a
29.7% speedup loss, and a gate that watches a third of the machine disappear without failing is
decoration. It now fails at 25%.

**4. A different fixture is refused, not compared.** The sweep records functions-per-crate and bundle
geometry in its `meta` record. If the candidate measured different work, the checker exits 2 with both
fixtures printed. Comparing 1500-function crates against 300-function crates would produce a
confident, meaningless verdict.

**5. Coverage is reported.** Core counts the baseline has and the candidate lacks are named and
skipped, and so are the widths that are not real cores on both sides (decision 2). No overlap at all
exits 2, "nothing comparable" is never a pass, because a green job that compared nothing is worse than
a red one.

**6. CI runs a reduced matrix; the full sweep stays local.** The job sweeps shape A on one core and on
two, the widths the recorded baseline holds as real cores (decision 2), at concurrency 1, 2 and 4, two
repeats per cell and four for the sequential cell (`REF_REPEATS`), because that one is every row's
denominator: 3 m 40 s measured on a six-core machine, and it covers the two ends of the curve, the
sequential baseline the ratios divide by, and the fill-the-machine case the policy is about. The full
matrix (both shapes, five core counts, six concurrencies) costs eighteen minutes measured and stays a
local or nightly job, because a gate nobody waits for gets skipped.

**7. The baseline is a reviewed file.** `docs/dev/tuning/data/scaling-sweep.json` is committed, so changing it is
a commit with the raw rows attached, visible in review. Re-baselining after a deliberate trade-off is
normal; re-baselining to make a red build green is visible in the diff. The candidate sweep and the
verdict JSON are uploaded as CI artifacts, so a disputed run can be re-examined without re-running it.


## The two tolerances this suite owes

The repo's headline numbers are frame budgets, 16.6 ms at 10,000 agents, 8 ms at 100,000 with 5,000
manifested, 6 ms at 1,000,000, and this gate cannot see any of them: it measures how well a build uses
cores, not how fast an NPC steers. Those budgets are enforced by the criterion benches and signed off in
`docs/dev/meta/release-checklist.md`, and the report says so in the reader's own terms rather than letting a green
badge imply more than it covers.

What this gate does owe, stated as numbers a reviewer can hold it to:

**Tolerance 1, parallel efficiency.** A cell's speedup may fall 12% below the recorded baseline before
CI warns, and 25% before it fails, both widened by the measured spread. In the units a developer feels,
on the CI matrix: the four-at-once cell on six cores runs in 4.1 s today, warns above 4.8 s and fails
above 5.5 s. Past that, the crawl has stopped filling the machine the way ADR 0033 promised, and the
build should stop.

**Tolerance 2, the CI budget.** The `Scaling regression` job carries `timeout-minutes: 15` against a
measured three minutes on six cores. If a change makes the sweep slower than that, the job is cut rather
than quietly taxing every push; the margin is five times the measurement, so the timeout fires on a
pathology, not on a busy runner.

Neither tolerance is invented for this document: 25% and 12% are the numbers `scripts/scaling_regression.py`
exports as `DEFAULT_FAIL_PCT` and `DEFAULT_WARN_PCT`, the report imports them rather than restating them,
and CI passes the defaults. An earlier draft of the report said "differences below 18%" while the gate
failed at 25%, one document disagreeing with its own gate, which is the failure mode this suite exists
to prevent.

## What this catches, and what it cannot

Catches: a scheduler change that stops filling cores, oversubscription that used to pay and no longer
does, a regression in how group execution interacts with process startup, and accidental drift in the
sweep's own fixture.

Does **not** catch a uniform slowdown, and this is structural rather than an oversight: the metric is
a ratio against the same-run core count's sequential run, so a change that makes every codegen 20%
slower cancels out. Per-task cost regressions belong to the existing criterion benches
(`crowd`, `packed`, `brain`, `tiers`, `band`), which compare absolute times on one machine and have
their own gates. Say it plainly in the report rather than implying the suite covers both.

## Consequences

- A green `Scaling regression` job means "the crawl still fills the machine as well as it did", and
  nothing more. The report states that in the same words.
- What CI can compare is bounded by the recorded baseline: today that is widths 1 and 2, because the
  wider rows of the baseline count hardware threads rather than cores (decision 2). The job says which
  widths it compared and which it left out, so a narrow gate is visible rather than assumed. A
  baseline re-recorded on a host whose topology is captured widens it again, and that is a deliberate
  commit with the numbers in the diff.
- Flakiness has a defined response: if the job wobbles, raise `REPEATS` (and `REF_REPEATS` for the
  denominator) before touching the percentages. Widening the thresholds is the wrong lever, past 12.5%
  of noise the failure line leaves the 25% the SLA promises, and the cell is reported unverifiable
  instead of quietly passing.
- `scripts/scaling_report.py --check` runs in CI: it regenerates the report in memory and fails if the
  committed copy differs, or if ADR 0034 stops stating the gate's formulas or the effective lines. Prose
  that restates a threshold is the one thing here that cannot be tested by running the code, so it is
  tested by comparing it to the code's output.
- The report (`docs/dev/tuning/reports/scaling-report.md`) is generated from the same baseline and imports the
  gate's thresholds, so the document a reviewer reads and the gate that protects it cannot drift apart.
  It prints the tolerances, what they mean in seconds for the CI's own cell, and the frame budgets this
  suite does not cover.

## Alternatives rejected

- **Gate on absolute wall clock.** Machine-dependent; would fail on any runner slower than this one.
- **Keep a baseline per machine class.** Combinatorial, and every new runner is an unbudgeted
  re-baseline.
- **Fit a model and compare parameters.** Tried during ADR 0033's figure: the model produced 165% and
  400% "efficiency" and had to be deleted. Ratios of measurements do not need a model.
- **Compare efficiency directly instead of speedup.** Efficiency divides by `min(cores, conc x threads)`,
  which at concurrency 1 is 1.0 for every machine, so the amplifier is noise, not information.
- **`n=2` with a t-test.** Two samples cannot support a distributional claim; the spread is used as a
  noise floor instead, and that limitation is stated rather than dressed up.
- **Compare widths regardless of how many cores the pin covered.** A four-wide row on a two-core
  machine with SMT measures hardware threads, not cores, so comparing it with a four-core row compares
  two different things. That is the failure this suite already had once: a 38% "regression" that was
  the runner's topology. The width is now what the machine's topology says it is, and a width that
  cannot be compared is named rather than failed.
- **Re-record the baseline to clear the red build.** The wider rows of the recorded baseline are
  ambiguous, so a new baseline on a host whose topology is captured would restore them. Doing that
  while the gate is red would trade a reviewed artifact for a green tick, which is the trade this
  document refuses: the baseline is re-recorded as a deliberate act, with the numbers in the diff.
