# ADR 0029, SIMD against the three-dimensional kernel: measured, and not adopted

**Status:** Rejected for the crowd kernel; the harness stays (2026-11-04)
**Related:** ADR 0023 (SIMD in the band kernel), ADR 0026 (cell-major crowd kernel), ADR 0028 (3D)

## The question

ADR 0023 vectorised the *band* kernel and won: four agents per block through `wide::f32x4`, about
30% on the 1M case. ADR 0028 then rewrote the *crowd* kernel in three dimensions and paid 19% for
it. So: does four-lane vectorisation pay in the 3D crowd kernel, and does it give the 19% back?

I built it and measured it. The answer is no, and the code is not in the tree.

## What was built

A `simd`-gated variant of `Crowd::scan`: the same node order, the same window, the same model, but
with a structure-of-arrays copy of the crowd (`x, y, z, vx, vy, vz, agent`) written during the
counting sort, so four neighbours are contiguous in memory and can be loaded without gathering.
The scan then masks four neighbours at a time, `keep = d² ≤ r² & id ≠ self`, weight by
`keep.select(1, 0)`, accumulate centres, headings and the `1/d²` separation push, with a scalar
tail of at most three neighbours per bucket. Parity was held two ways: `steer_reference` kept the
scalar road reachable in the same process, and a test asserted identical `steered`, `isolated`,
`cells` and `pairs`, and desired velocities within 1e-4.

## The numbers

10,000 agents, `crowd_kernel` group in `benches/crowd.rs`, this machine, medians:

| Crowd | Scalar | SIMD | Delta |
| --- | --- | --- | --- |
| planar (1.6 m spiral) | 7.25 ms | 8.40 ms | **+16%** |
| volumetric (20 m of height) | 6.01 ms | 7.56 ms | **+26%** |
| dense (10,000 in a 30 m disc) | 107.9 ms | 117.0 ms | **+8%** |

With `RUSTFLAGS="-C target-cpu=native"` (AVX2 available) the picture does not change: scalar 7.14 /
6.01 / 108.0 ms, SIMD 8.37 / 7.63 / 118.0 ms. The SIMD road is slower in every case, including the
one built to favour it.

## Why the band kernel wins and this one loses

Three reasons, and only the first is about the machine:

1. **The scalar scan rejects neighbours before doing any work.** A neighbour outside the radius
   costs one compare and a branch; inside, it costs the accumulation. Masked SIMD computes the
   accumulation for all four lanes regardless and throws away the failures, so it trades cheap
   rejected comparisons for expensive wasted arithmetic. That is the wrong trade for a crowd whose
   neighbourhoods are mostly empty.
2. **The band kernel's block is four agents sharing one neighbour**, so one load of the neighbour
   feeds four lanes of real work and the cell walk is amortised four ways. Here the lanes are four
   neighbours of one agent: no reuse, and the block is only full when a bucket holds four or more
   agents. At the default density a bucket holds about two.
3. **The SoA copy is a whole extra pass** over 10,000 agents and six components per frame, paid by
   every case whether or not any bucket fills a block.

Even the dense case, 40-odd neighbours each, where lanes are nearly always full, loses 8%. That
is the strongest evidence: the win in ADR 0023 was about sharing a neighbour across lanes, not
about vector arithmetic as such.

## The decision

The vectorised scan is removed, not merely defaulted off. Shipping code that is measurably slower
with a flag in front of it is how a "fast path" becomes a foot-gun that someone enables in
production because the name says `simd`.

What stays is the ability to ask the question again:

- the `crowd_kernel` benchmark group, three crowds (planar, volumetric, dense), one line to run:
  `cargo bench --bench crowd -- crowd_kernel` and the same with `--features simd` once a variant
  exists;
- this ADR, with the numbers and the reason.

The `simd` feature still exists and still means what ADR 0023 says: it vectorises the *band* kernel,
where it wins, and it does nothing to the crowd kernel.

## If it is tried again

The shape that could work is the band kernel's: block four agents of the same cell, walk the shared
neighbour window once, and broadcast each neighbour into four lanes that are all doing real work.
That needs agents clustered four-deep in a cell, which the default density does not provide and a
hand-tuned cell size might. It would be a different algorithm, not a vectorised loop, and it should
be justified by a profile showing the scan dominates, at 9.18 ms for 10,000 agents the scan does,
but the waste is in the rejection rate rather than in the arithmetic width.
