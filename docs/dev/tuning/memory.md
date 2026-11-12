# Memory profile, the packed kernel, scalar and wide

Measured 2026-11-04, sandbox x86_64, `memory_probe` example, `--release`.

The question this answers: the SIMD change was sold on wall-clock time, but it also changed
*where* the kernel touches memory (a cell-major order, four lanes per walk). Does it reduce
heap traffic, or cache misses, or both, or neither?

## Method

```sh
cargo build --release --example memory_probe                 # scalar
cargo build --release --example memory_probe --features simd # wide
valgrind --tool=cachegrind --cache-sim=yes ./memory_probe 20000 5
```

`examples/memory_probe.rs` counts allocations with a global allocator, seeds 50,000 agents
from a fixed PRNG, warms the pack, then runs N frames of `steer` + `integrate` + `advance`
and prints allocations, bytes and milliseconds. Cachegrind runs the same binary with 20,000
agents and 5 frames, which is slow under instrumentation and enough to be conclusive.

## Heap

| Kernel | Allocations over 60 frames | Bytes | ms/frame @ 50,000, extent 512 |
| --- | --- | --- | --- |
| scalar | **0** | **0** | 65.7 |
| wide | **0** | **0** | 27.4 |

Neither kernel allocates in the steady state, which is the invariant the whole design rests
on: the pack grows only through `push`, and `steer` reuses its grid, chains and order vector.
The SIMD path costs a fixed 4 bytes per agent for the cell-major order (`order: Vec<u32>`),
taking the per-agent footprint from 28 to 32 bytes:

| Agents | Arrays |
| --- | --- |
| 50,000 | 1,562 KiB |
| 1,000,000 | 30.5 MiB |

At a million agents that is 32 MB of steering state, against the 4 MB the order vector adds.

### The order vector was not SIMD-only until 2026-11-12

The row above said the scalar path carries 28 bytes per agent. That was the intent, not the
code: `order` was declared, allocated and filled unconditionally, and only `steer_blocks`
read it. A scalar build paid 4 bytes per agent **and** an O(n) pass per `steer` for a vector
it never looked at.

`order` is now behind `#[cfg(feature = "simd")]`, along with its allocation and its fill.
The measured footprints, from `memory_probe`:

| Kernel | Bytes/agent | ms/frame @ 50,000, extent 512 |
| --- | --- | --- |
| scalar | 28 | 42.2 |
| wide | 32 | 14.6 |

The pair counts match to the digit between the two builds (`135082756`), so gating the
vector changed the footprint and the time without changing the result.
`the_per_agent_footprint_matches_the_kernel` in `bevy-flock-core` holds the figure to the
kernel that pays for it.

## Cache

Cachegrind, 20,000 agents × 5 frames (includes setup, so treat the ratios as the finding and
the absolutes as indicative):

| Event | scalar | wide | Change |
| --- | --- | --- | --- |
| Instructions | 143.3 M | 128.6 M | −10.3% |
| Data reads | 15.63 M | 17.33 M | **+10.9%** |
| Data writes | 1.23 M | 2.61 M | +113% |
| D1 read misses | 12.23 M | 2.72 M | **−77.7%** |
| D1 write misses | 0.11 M | 0.20 M | +94% |
| D1 total misses | 12.33 M | 2.93 M | **−76.3%** |
| LL read misses | 3.28 M | 0.34 M | **−89.6%** |
| LL total misses | 3.34 M | 0.47 M | **−85.9%** |
| D1 miss rate | 73.1% | 14.9% | |
| LL miss rate | 19.8% | 2.4% | |

## What this actually says

The wide kernel **executes 10% fewer instructions and issues 11% more memory operations**, and
still finishes 2.4× sooner. The instructions were never the bottleneck. What changed is the
order: a block of four agents now walks one cell's chain instead of four agents walking nine
cells each in insertion order, so the same 64-byte line serves four lanes rather than one
agent, and the positions it reads are contiguous instead of scattered across the array.

The 73% D1 miss rate on the scalar path is the number worth remembering. It means the old
kernel spent three quarters of its data references *missing the L1 cache*, a linked list of
indices walked in insertion order, re-reading positions that lie in random array order. The
arithmetic in that kernel was almost irrelevant; the layout was everything.

This is why ADR 0022's profile showed `Pack::steer` at 93% self time and why removing
transcendentals bought only 5%: the transcendental work was visible in the profile because the
rest of the kernel was waiting on memory and sampling as stalls inside the same symbol.

## Consequences for the next round

1. **Do not chase vector width further.** `f32x8` would halve the instruction count of a phase
   that is now 15% miss-bound and 10% instruction-bound. The measured headroom is in locality.
2. **The chain is the next target.** A CSR layout (counting pass, then contiguous neighbour
   ranges) would replace the linked-list walk with a sequential read; the 2.4% LL miss rate
   says the destination is worth reaching.
3. **Watch the write side.** D1 write misses nearly doubled, because `desired` is written in
   cell-major order rather than index order. At 1M agents that is 8 MB written in a scattered
   pattern per tick. If the next profile shows write-allocate traffic, the fix is a copy pass
   that reorders writes, not a faster kernel.

## Reaching a behaviour tree

The store answers "what does this NPC remember about that person" in `O(subjects)`. A tree
asks it several times a tick from arbitrary branches, so the answer is lifted out once:
`adapters::primary::memory_bridge` writes the strongest subject's valence, familiarity and
encounter flags into three blackboard slots (`KEY_MEMORY_*`), and
`behavior::memory_leaves::Remembered::read` turns them into one small struct with three
predicates, `is_known`, `is_friend`, `is_grudge`.

Three rules make the view safe to read blindly:

- **Familiarity gates it.** A valence without familiarity is not a memory, so a decayed
  grudge cannot turn every stranger into an enemy.
- **Flags outrank intensity.** `Attacked` and `Robbed` survive a decay that softened the
  valence: a grudge is an event, not a feeling.
- **Absence means stranger.** No slots is `None`, not zeroes, so "I have never met you" and
  "I am indifferent to you" stay distinct.

Enrolment is by proximity and budget (`MemoryConfig::publish_radius`, `enrol_budget`), so
the rows go to the NPCs the player can actually meet, nearest first. When the store fills,
`MemoryBridgeStats::budget_bound` says so instead of the bridge evicting the relationship
the player spent longest building. Cleared views are counted separately: "the town forgot
me" and "the town never knew me" are different bugs.
