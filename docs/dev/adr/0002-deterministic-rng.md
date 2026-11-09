# ADR 0002 — A PCG32 stream per NPC, derived from `(seed, NpcId)`

- Status: Accepted
- Date: 2026-11-04

## Context

Every NPC needs randomness: personality, appearance, dialogue choices, which way
to drift when idle. The obvious implementation is a shared RNG resource. It is
also the one that makes replays useless, because the value NPC #517 receives
depends on how many entities happened to be spawned before it, in what order, on
which thread.

Two properties are required:

1. **Reproducibility.** The same seed produces the same world.
2. **Isolation.** Adding, removing or despawning an NPC must not change anyone
   else's draws.

Property 2 is the one that is usually missed, and it is the one that makes a
replay of "the bug happened at tick 40,112" possible.

## Decision

`RngStream::for_npc(seed, id)` is a pure function: it mixes the two through
`SplitMix64` to pick a 64-bit stream selector, and seeds PCG32 with the selector
as both the initial state and the increment. Each NPC owns its stream.

The algorithm is PCG32 (XSH-RR, 64/32), verified against the reference C
implementation for ten thousand draws and pinned by a golden vector.

## Consequences

- Adding an NPC cannot perturb another's sequence, because no NPC ever advances
  anyone else's stream.
- Streams are resumable: `state_bits()` and `from_state()` exist so snapshots can
  serialise exactly where a stream was.
- One 24-byte struct per NPC. At 100,000 NPCs that is 2.4 MB, which is the price
  of the property above and is cheaper than any solution involving a lookup table.
- The golden vector is a load-bearing test: changing the seed derivation breaks
  every stored replay, and the test says so in its failure message.

## Alternatives rejected

- **One global `RngStream` resource** — fast, simple, and fails property 2
  completely.
- **`rand::rngs::StdRng`** — no stability guarantee across `rand` versions; a
  dependency bump would silently invalidate every replay. `rand_pcg` would work
  but adds a dependency and a version-stability promise we would rather own.
- **`Hash(NpcId) ^ seed` as the stream seed** — correlated streams for
  neighbouring identifiers; tested against, and it fails the independence test.
