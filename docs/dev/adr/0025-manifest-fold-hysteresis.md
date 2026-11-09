# ADR 0025 — The manifest window is a disc inside the fold radius

- **Status:** Accepted
- **Date:** 2026-11-04
- **Fixes:** the churn finding recorded in `docs/dev/tuning/gates/continuum.md`
- **Related:** ADR 0018 (ambient continuum), ADR 0020 (band plugin), ADR 0021 (multi-band)

## Context

The band has two radii: it manifests density inside the manifest radius and folds entities back
into density beyond the fold radius, which is `fold_slack` (1.15) times larger. The gap between
them is hysteresis, and the intent is that people are only ever manifested onto ground they can
stay on.

It did not work. The recorded finding was that the band manifests 256 agents a tick and folds
256 the next, forever, with population conserved and every gate passing. Reproduced:

```text
tick  0: live   256 manifested + 256 folded +   0 density 39744
tick  1: live   256 manifested + 256 folded + 256 density 39744
...
tick 11: live   256 manifested + 256 folded + 256 density 39744
live agents already outside the fold radius: 256
```

The band never reached its 5,000 target. It held exactly one ration of agents, all of them
standing outside the fold bound, and spent every frame re-manifesting the same people it had
just folded.

## Three causes, found by measurement

1. **The manifest window was square.** It walked Chebyshev rings out to
   `ceil(radius / spacing)` cells, which reaches `radius * sqrt(2)` into the corners: 1.41
   radius against a 1.15 radius fold bound. At the default geometry, 2,576 of the 12,517 cells
   the window visited were between the two radii and **1,548 were beyond the fold radius** — so
   a spawn there was born outside the band and folded on the next tick.
2. **`seed_field` filled the field from the origin and stopped when it ran out.** 40,000 people
   over a 256×256 field is 0.61 per cell, so the utility piled them into the first forty
   thousand cells in row-major order: everything within about 150 rows of the origin. A camera
   at the centre of the map stood in empty country, the manifest found nothing within its
   radius, and the *square* window kept reaching until it found someone — usually far away.
3. **Nothing checked the spawn point against the fold bound at all.** Even a correct window
   would have let `fold_slack: 1.0` spawn agents exactly on the boundary, to be folded as soon
   as they were steered.

Cause 2 is why the defect was invisible in production-shaped tests: the band behaves correctly
when there is density near the camera, and every fixture that used `seed_field` had none.

## Decision

**Eligibility is a property of the spawn point, not of the storage cell.** The manifest:

- walks Chebyshev rings for iteration order only, and **skips any cell whose centre is farther
  than `radius` from the focus** (squared distance, no square root);
- skips any cell whose centre is at or beyond `fold_radius`, which only bites when a caller
  sets `fold_slack` to 1.0 — nobody is manifested onto ground they would immediately leave;
- breaks out of the ring walk once even the nearest point of a shell exceeds the radius;
- spawns at the *same* point it vetted, so the check and the spawn cannot disagree.

`seed_field` **distributes uniformly** over the whole field by a Bresenham share over cell
indices: every cell gets `count / cells`, and the remainder is spread evenly across the field
rather than consumed from the origin.

The invariant, stated once: **a manifested agent always appears strictly inside the manifest
radius, and the fold radius is never smaller.** The hysteresis gap is what makes the two systems
stop arguing; anything that breaks the inequality reintroduces the oscillation.

## Measurements

After the fix, same configuration, same fixture:

| | Before | After |
| --- | --- | --- |
| Live agents at tick 11 | 256 | 3,072 |
| Live agents at tick 20 | 256 | **5,000** |
| Folds per tick after convergence | 256 | **0** |
| Manifestations per tick after convergence | 256 | **0** |
| Live agents outside the fold radius | 256 | **0** |

Convergence takes twenty frames at the default ration (5,000 / 256), then the band is quiet:
sixty consecutive frames with zero manifestations and zero folds, population conserved at
40,000, and the 1,000,000-inhabitant frame unchanged at 3.80 ms (it was 3.78 ms when part of
that frame was churn, so the band is now doing the same work without the waste).

## Consequences

- `tests/band_churn.rs` pins all of it: convergence to target, zero churn for sixty frames after
  convergence, no agent ever manifested outside the band, a zero-slack band that never folds
  what it manifests, and uniform seeding.
- Gate 9 no longer needs its `target: 0` sidestep: it runs against a full band with the camera
  held still, and asserts that exactly one agent folds, that density gains exactly one person,
  and that no resident is disturbed. The sidestep existed only because the churn ate the ration.
- The seeding change is a behavioural change for anyone relying on `seed_field`: density is now
  where the field is, not where the loop started. That is the documented intent of the function,
  but it will move fixture numbers.
- Gates 4, 5, 6 and 7 were re-run through the 10,000-tick harness after the change and still
  pass; the fix narrows the band's reach, so it can only reduce crossings, not create them.
