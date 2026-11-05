# ADR 0034 — A regression suite for group scaling, and the report it produces

**Status:** Accepted (2026-11-05)
**Context:** ADR 0033 states the capping policy, ADR 0032 and `docs/performance.md` carry the
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
four-core runner. A drop means the machine used its cores less well than before — on any hardware.

**2. Noise sets the threshold, and the threshold moves.** Every cell is the median of `REPEATS` runs
(schema 2 records the spread between them). A threshold tighter than the spread is not signal, so:

```
noise   = max(8%, baseline spread, candidate spread)
fail_at = max(--fail-pct default 25%, 2 x noise)
warn_at = max(--warn-pct default 12%, noise)
```

CI passes the same defaults. The first draft used 35% and 18%, and a dry run showed why that was too
forgiving: a deliberately injected 45% wall-clock slowdown on one cell only warned, because 45% more
wall clock is a 29.7% speedup loss. At 25% it fails, and a runner whose spreads are wider than 12.5%
widens the threshold to twice its own noise instead of arguing about it. A cell that fails names its shape, core count, concurrency and the absolute
percentage it lost, so the log says what broke rather than "regression".

**3. A different fixture is refused, not compared.** The sweep records functions-per-crate and bundle
geometry in its `meta` record. If the candidate measured different work, the checker exits 2 with both
fixtures printed. Comparing 1500-function crates against 300-function crates would produce a
confident, meaningless verdict.

**4. Coverage is reported.** Core counts the baseline has and the candidate lacks are named and
skipped. No overlap at all exits 2 — "nothing comparable" is never a pass, because a green job that
compared nothing is worse than a red one.

**5. CI runs a reduced matrix; the full sweep stays local.** The job sweeps shape A on one core and on
the runner's own `nproc`, at concurrency 1, 2 and 4, two repeats per cell: a few minutes, and it
covers the two ends of the curve — the sequential baseline the ratios divide by, and the
fill-the-machine case the policy is about. The full matrix (both shapes, five core counts, six
concurrencies) costs about ten minutes and stays a local or nightly job, because a gate nobody waits
for gets skipped.

**6. The baseline is a reviewed file.** `docs/data/scaling-sweep.json` is committed, so changing it is
a commit with the raw rows attached, visible in review. Re-baselining after a deliberate trade-off is
normal; re-baselining to make a red build green is visible in the diff. The candidate sweep and the
verdict JSON are uploaded as CI artifacts, so a disputed run can be re-examined without re-running it.

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
- Flakiness has a defined response: if the job wobbles, raise `REPEATS` (noise is measured, so the
  thresholds widen automatically) before touching the percentages.
- The report (`docs/reports/scaling-report.md`) is generated from the same baseline, so the document a
  reviewer reads and the gate that protects it cannot drift apart.

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
