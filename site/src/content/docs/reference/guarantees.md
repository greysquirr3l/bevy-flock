---
title: Guarantees
description: Every promise this crate makes, and the test that fails when it stops being true.
---

A guarantee without a test is a preference. Each claim below names the test that enforces it, so you can run the test
instead of trusting the sentence. Run `cargo test --workspace` and they all execute.

| guarantee | enforced by |
| --- | --- |
| Same seed, same features, same platform give the same state digest over hundreds of ticks | `tests/determinism.rs` |
| A spatial build produces byte-identical cell windows and order across worker counts | `spatial::every_thread_count_builds_identical_windows`, `tests/spatial_parity.rs` |
| Population is conserved on every tick, including mid-transition | `tests/band_continuity.rs::gate_4_population_is_conserved_on_every_tick` |
| Crossing a band boundary never pops: the visible count moves by less than 2% | `tests/band_continuity.rs::gate_5_the_boundary_never_pops` |
| An anchor at 10 km survives 10,000 ticks without folding | `tests/band_continuity.rs::gate_6_an_anchor_survives_ten_thousand_ticks_at_ten_kilometres` |
| A steady-state frame allocates nothing | `tests/band_continuity.rs::gate_7_a_steady_state_frame_does_not_allocate`, `tests/crowd_no_alloc.rs`, `tests/spatial_batch_no_alloc.rs`, `tests/tree_no_alloc.rs` |
| Folding removes the entity and adds exactly one person to the field | `tests/band_continuity.rs::gate_9_folding_removes_the_entity_and_adds_exactly_one_person` |
| The core crate names no Bevy engine crate | `tests/core_boundary.rs` |
| The core works with no engine in the dependency graph | `crates/bevy-flock-core/tests/headless_consumer.rs` |
| A save written by the previous release still loads | `tests/save_compat.rs` |
| Social memory stays bounded as ticks accumulate | `tests/memory_aging.rs` |

## How to read this table

The spatial parity claim is the one people assume is false, because most parallel simulators give up byte equality for
speed. It holds here because every worker owns a disjoint slot range and the kernel never reduces across workers.

The allocation claims are made with a counting allocator, not with a profiler. `tests/crowd_no_alloc.rs` installs one,
warms the frame up, and asserts the counter does not move over the steady-state frames. If you change a system that
allocates lazily, that test is what notices. `tests/band_continuity.rs::gate_7_...` does the same for a whole band
frame: it counts the thread the frame runs on, proves the counter by counting a deliberate allocation before it reads
anything, and then asserts that 600 steady frames allocate nothing at all.

## The limits of these guarantees

- **Determinism is per platform and per feature set.** Floating point is not portable across architectures, and enabling
  `simd` changes the order of operations inside a lane group. Pin both in a replay.
- **Parity is across worker counts on one machine, not across machines.** A 4-core laptop and a 64-core server produce
  the same bytes because the partitioning is fixed, but the timing differs, and this crate does not promise timing.
- **`memory` changes the digest.** A save from a build with memory enabled is a different simulation from one without it,
  which is why the feature is in the determinism contract rather than an implementation detail.

If a guarantee above is not covered by a test you can run, that is a bug in this page. Open an issue with the row.
