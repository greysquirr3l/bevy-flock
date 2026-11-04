# Architecture decision records

One file per decision that would be expensive to reverse. Each records the
context, the decision, its consequences, and the alternatives that were rejected
with the reason — the last part is the one that stops a decision being re-litigated
every six months.

A decision is not edited after the fact. If it is reversed, a new ADR supersedes
it and both stay in the tree.

## Index

| ADR | Decision | Status |
| --- | --- | --- |
| [0001](0001-hexagonal-layout.md) | Collapsed hexagonal layout: `domain`, `application`, adapters | Accepted |
| [0002](0002-deterministic-rng.md) | A PCG32 stream per NPC derived from `(seed, NpcId)` | Accepted |
| [0003](0003-spatial-grid.md) | Dense uniform grid with a cell budget; two measured designs rejected | Accepted |
| [0004](0004-libm-transcendentals.md) | `libm` instead of `std` maths in simulation code | Accepted |
| [0005](0005-budgeted-spawning.md) | Budgeted spawning from a queue of ranges | Accepted |
| [0006](0006-trees-and-state-machines.md) | Behaviour trees and state machines as separate primitives | Accepted |
| [0007](0007-no-renderer.md) | No renderer, physics or networking in the core | Accepted |
| [0008](0008-fallible-constructors.md) | Parse, don't validate; no panic paths in library code | Accepted |
| [0009](0009-virtual-cost-stride-scheduler.md) | Cognition scheduled against a virtual cost by start-time fair queueing | Accepted |
| [0010](0010-social-memory.md) | Social memory as a bounded, integer, two-tier store | Accepted |
| [0011](0011-memory-and-the-scheduler.md) | Memory enters the scheduler as cost and weight, never as a tier | Accepted |
| [0012](0012-memory-save-format.md) | Relationships persist as ages in a versioned binary record | Accepted |
| [0013](0013-subject-registry.md) | Subjects are matched by stable key, and the file says which keys it used | Accepted |
| [0014](0014-compatibility-testing.md) | Save compatibility is tested against committed files and a pinned old build | Accepted |
| [0015](0015-memory-diagnostics.md) | Waste is measured in integers and drawn outside the library | Accepted |
| [0016](0016-subject-ceiling.md) | Sixty-four subjects, and a footprint games can budget from | Accepted |
| [0017](0017-tiered-cognition.md) | Tiered cognition: from reptile to social | Proposed |
| [0018](0018-ambient-continuum.md) | Ambient continuum: population as density past the manifest band | Proposed |
| [0019](0019-memory-aging-and-anchors.md) | Memory aging for NPCs nobody simulates | Accepted |

## Reading order

For a new contributor: 0001 gives the shape of the code, 0002 and 0004 explain why
the numbers are reproducible, 0003 explains the one data structure that dominates
frame time, and 0009 explains how cognition will be bounded without giving up
replay.

## Writing one

- Name the alternative you rejected, and why. "We chose X" is not a decision
  record; "we chose X because Y lost for this reason" is.
- Include the numbers if a measurement drove the decision. A benchmark table ages
  better than an adjective.
- State the consequences you do not like. An ADR with no downsides is an
  advertisement.
