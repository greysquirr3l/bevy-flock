# ADR 0024 — The packed layout, the ordering constraints, and how parity is proven

- **Status:** Accepted
- **Date:** 2026-11-04
- **Related:** ADR 0018 (packed kernel), ADR 0023 (wide kernel), `docs/memory.md`

## Context

ADR 0023 decided *that* the steering kernel runs four agents at a time. It did not record the
three things a library has to be explicit about once a layout is load-bearing:

1. what the layout **is**, down to bytes per agent, because it is now the crate's performance
   contract;
2. what the cell-major order **constrains**, because it is derived state that a caller can
   invalidate;
3. how the wide path is **proven** to agree with the scalar one, because "it looks the same in
   a screenshot" is not a guarantee a game can ship.

The memory profile (`docs/memory.md`) settled the first one empirically: the win is locality,
not vector width. That changes what the crate should promise.

## Decision

### 1. The layout is structure-of-arrays, 32 bytes per agent

| Array | Type | Bytes/agent |
| --- | --- | --- |
| `positions` | `Vec<[f32; 2]>` | 8 |
| `velocities` | `Vec<[f32; 2]>` | 8 |
| `desired` | `Vec<[f32; 2]>` | 8 |
| `next` | `Vec<i32>` (bucket chain) | 4 |
| `order` | `Vec<u32>` (cell-major) | 4 |
| **total** | | **32** |

Plus the grid heads: one `i32` per cell, `(extent / radius)²` cells, allocated once.

A million agents therefore cost 30.5 MiB of steering state. This is a documented number, not
an implementation detail: it is the crate's answer to "what does bevy-flock cost me per NPC",
and it is quoted in the README and the milestone sign-off.

Array-of-structures (`Vec<Agent>` with a 24-byte struct) would be simpler and was rejected.
The wide path loads four lanes with four scalar loads because the fields are already separated;
an interleaved struct would need a gather per candidate, which is exactly the operation the
measured cache profile says we cannot afford. This rejection is *argued*, not measured — the
SoA layout was chosen before the SIMD work existed, for the scalar kernel's sake, and the
cache profile has since confirmed it.

### 2. The order is derived, never authoritative

`order` is rebuilt at the top of every `steer` call as a by-product of the grid walk. The
constraints this imposes, and the guarantees that follow:

- **Indices are stable for the life of the pack.** `push` appends; nothing renumbers. A game
  that stores `NpcId` → index mappings keeps them.
- **A caller may mutate positions between ticks**, including teleporting an agent. The order is
  rebuilt, so this is not corrupting — at worst a block is no longer spatially compact and
  falls back to the scalar path for that block.
- **Determinism requires the same insertion order and the same parameters.** The order is
  derived from the grid, which is derived from positions, which are integer-cell-quantised;
  two runs with the same inputs produce the same order, so the same blocks, so the same bits.
- **The order is not an API.** It is not exposed, and no caller may depend on block boundaries.
  Anything that reads it from outside the crate is a bug in waiting.
- **Positions must be finite.** `cell_of` clamps non-finite input into `0..side` rather than
  panicking, because a crowd system's failure mode should be a wrong frame, not a dead game.
  This is load-bearing for the fallback rule, not an accident.

### 3. Parity is proven by construction, not by inspection

The scalar kernel is retained permanently as `steer_one`, and it is the **definition** the wide
path must reproduce:

```text
the_vectorised_kernel_agrees_with_the_scalar_one
    builds two identical 256-agent packs from a fixed PRNG
    steers one through the wide path, one through steer_one
    asserts bit equality of every desired velocity
    asserts equality of pairs, steered and isolated
```

Rules that follow, and that a contributor will be held to:

- **No `mul_add` on a parity path.** A fused multiply-add rounds once where the scalar path
  rounds twice; that is a one-ULP divergence and it fails the test. The width is worth less
  than the guarantee. (This is how the divergence was found on the very first run.)
- **Masked-out contributions must be exactly zero**, so a lane accumulates the same neighbours
  in the same order. `select(one, zero)` is exact; a masked add of `0.0` cannot perturb.
- **New steering rules go into `steer_one` first**, then get mirrored in `scan_block`. The test
  is the specification; if the wide path drifts, CI says so on the same commit.
- **Both feature sets run in CI.** The scalar build proves the reference still works; the
  `simd` build proves the mirror still matches it.

## Measurements

From `docs/memory.md`, 20,000 agents × 5 frames under cachegrind:

| Event | scalar | wide |
| --- | --- | --- |
| D1 miss rate | 73.1% | **14.9%** |
| LL miss rate | 19.8% | **2.4%** |
| Instructions | 143.3 M | 128.6 M |
| Data reads | 15.63 M | 17.33 M |

Fewer instructions, *more* memory operations, 2.4× the throughput. The ordering is the
optimisation; the vector width is a multiplier on top of it.

## Consequences

- The crate has a stated cost per agent (32 bytes) and a stated shape (SoA). Both are public
  promises and both are cheap to keep.
- The wide path can never silently rot: the scalar path is tested against it every run, in the
  same process, on the same data.
- A future CSR layout (see `docs/memory.md`) would change the byte count and the chain, but not
  the parity rule: the scalar path remains the definition and the test remains the arbiter.
- Performance work from here should be justified against the cache profile, not the instruction
  profile. "Fewer instructions" is no longer an argument.
