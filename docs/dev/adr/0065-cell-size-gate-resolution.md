# 0065. The cell-size gate judges the frame it was written for, and only at a resolution it can resolve

- Status: accepted
- Date: 2026-11-11
- Supersedes: none. Amends the expiry condition recorded in [0031](0031-cell-sizing-for-lanes.md).

## Context

ADR 0031 left `MAX_CELLS` at `2^20` (13.5 m cells in a one-kilometre volume) and attached an expiry condition: if
tiering ever raises the steered share of the crowd, a finer grid would start to pay for a frame, and the numbers in
`flock_frame_cell` would say so. `scripts/cell_size_gate.py` is that condition, wired into CI as "Cell-size re-open gate
(ADR 0031)": it reads `target/criterion/flock_frame_cell`, compares the 9 m case against the 13.5 m case, warns at a 3%
whole-frame saving and fails at 10%.

It has started failing, and it has started flipping between runs. One session reported 14.9% at 10,000 agents on one host,
5.6% on a branch and 7.2% on `main` on another, and could not reproduce any of them. A gate that cannot reproduce its own
verdict is not measuring the thing it names, so the question is not "is the threshold too tight" but "what is this gate
able to see, and whose frame is it judging".

## What was measured

Same workload, same binary, same host, three minutes apart, two consecutive runs of
`cargo bench --offline --bench crowd -- flock_frame_cell` on a six-core aarch64 VM. Saving is 13.5 m against 9 m, from the
criterion point estimates:

| population | run 1 | run 2 | verdict the gate produced |
| --- | --- | --- | --- |
| 10,000 | −0.3% | −1.1% | pass |
| 100,000 | +0.0% | +2.1% | pass, two points apart on an identical tree |

The other host's numbers for the same comparison, as reported: 14.9% at 10,000, 7.2% and 5.6% on a branch and on `main`.
Five measurements of one quantity, spanning 0% to 15%, with no host or branch explaining the spread.

The reason is in the harness, not the grid. `benches/crowd.rs` runs this group at `sample_size(10)`, so criterion's 95%
interval on the 100,000-agent case is 46.4 ms to 49.0 ms: 5% wide, on a case whose whole question is a 10% difference. Two
configurations drawn from that interval land anywhere within it, which is why a run can report 0.0% and the next reports
2.1%, and why a loaded runner can report 14.9%. The fail line sits inside the noise band, so the gate fails on the runner
rather than on the grid.

The second defect is whose frame it judges. `rows()` returns both populations and `main()` takes the maximum saving, so a
10,000-agent frame decides a decision ADR 0031 made about the 100,000-agent tiering case. At 10,000 agents the finer grid
does not win in either of the runs above, and if it did win it would be for a different reason than ADR 0031 weighed
(tighter candidate boxes in the query, not the steered share), so it cannot answer the question the gate asks.

## Decision

1. **The gate judges the 100,000-agent case.** That is the population ADR 0031 reasoned about, and the only one where the
   expiry condition means what it says. The 10,000-agent case stays in the table as information and cannot fail a run.
2. **The gate fails only on a difference it can resolve.** A failure now needs the saving to clear the fail threshold
   *and* the two cases' 95% intervals to be disjoint, which is criterion's own test of whether a difference exists.
   A saving above the line with overlapping intervals warns and passes, because that is a run that needs repeating rather
   than a decision that has been overturned.
3. **The harness is raised to a resolution the gate needs.** This group moves from `sample_size(10)` to
   `sample_size(50)`. At roughly 46 ms per iteration that is about 2.3 seconds of measurement per case, and it narrows the
   interval by roughly a factor of two, so a real 10% difference becomes visible and a 2% wobble stops being reported as
   one.
4. **`MAX_CELLS` stays at `2^20`.** Nothing measured here justifies moving it, and the numbers that would justify an
   adaptive budget (cells per body at 1k, 10k and 100k, interleaved, 50 samples) do not exist yet. The reopen condition is
   recorded below instead of guessed at.

## The reopen condition, restated

`MAX_CELLS` should stop being a constant when a measured sweep shows that a budget derived from the population beats the
fixed one on the frame, at two populations at least, with disjoint intervals. The experiment is a `cells_per_body` sweep
rather than a cell-size sweep, because a cell size that is right for a million bodies is four times too coarse for ten
thousand, and the current default is chosen for the largest crowd the library supports. That sweep invalidates every
recorded frame number in `performance.md`, so it needs its own plan task and its own re-measurement, not a threshold
adjustment in a gate.

## Outcome

Recorded from CI run `43004961498` on `6657104`, the first green run with this gate in place:

| case | 13.5 m | 9 m | verdict |
| --- | --- | --- | --- |
| 10,000 | [7.34, 7.88] ms | [7.26, 7.82] ms | reported, intervals overlapping |
| 100,000 | [59.48, 59.95] ms | [59.37, 59.78] ms | judged, intervals overlapping, 0.2% |

`PASS: 9 m cells save 0.2% of a 100000-agent frame, below the 3% warn threshold.` The same run's scaling regression
held all four comparable cells at −2.8%, −4.4%, +3.4% and −2.9%, so the two gates now agree about what a difference of a
few percent means on a shared runner.

The CI host is slower than the machine this ADR was written on (59.5 ms against 47.6 ms for the same frame), which is the
other reason the gate reads intervals from one run rather than a number from a document: a percentage of a different
frame is not the same quantity.

The sibling gate made the same move in `6657104`, replacing ADR 0034's observed range with a robust standard error on the
median. One asymmetry between them is deliberate and should survive any future harmonisation. That gate asserts a recorded
baseline still holds, so a cell it cannot resolve is a failure of evidence and exits 2. This gate asserts an expiry
condition on a design decision, so a difference it cannot resolve is the absence of evidence to re-open the decision, and
passes with a warning. Same arithmetic, different question.

## Consequences

- CI stops failing on a quantity it cannot resolve, and the failing job on `main` is explained rather than tolerated.
- The warn band changes meaning: it now says "above 3% and resolvable", so a warning is worth reading.
- The gate grows a `--self-test` with fixtures for the four cases that matter (a resolvable fail, a fail-sized difference
  inside the noise, a large saving at the unjudged population, and no results at all), because a gate whose verdicts
  nobody has tested is how this happened.
- Bench time for this group rises by about seven seconds. That is the cost of asking a resolvable question.
