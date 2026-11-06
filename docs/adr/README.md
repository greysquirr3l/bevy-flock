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
| [0020](0020-band-plugin.md) | The manifest band plugin | Proposed |
| [0021](0021-multi-band.md) | Multi-band scaling past 250,000 active entities | Proposed |

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
| [0023](0023-simd-steering.md) | Four-agent SIMD steering behind a cell-major order | Accepted |
| [0024](0024-layout-ordering-and-parity.md) | The packed layout, the ordering constraints, and how parity is proven | Accepted |
| [0025](0025-manifest-fold-hysteresis.md) | The manifest window is a disc inside the fold radius | Accepted |
| [0026](0026-cell-major-crowd-steering.md) | Crowd steering walks cells once, in arrays, instead of entities three times | Accepted |
| [0027](0027-spawn-placement.md) | Spawn placement is a validated type: spiral, random, or authored points | Accepted |
| [0028](0028-three-dimensional-crowds.md) | The crowd kernel and spawn placement are three-dimensional | Accepted |
| [0029](0029-simd-in-three-dimensions.md) | SIMD in the 3D crowd kernel was measured, and rejected | Rejected |
| [0030](0030-transposed-simd.md) | Transposed SIMD (lanes are agents) wins 21-29%; adopted behind `simd` | Accepted |
| [0031](0031-cell-sizing-for-lanes.md) | Cell sizing wants 6-10 agents per bucket; transposed road is the default | Accepted |
| [0032](0032-concurrent-plan-tasks.md) | Concurrent plan tasks: build cache safe, git index not | Accepted |
| [0033](0033-group-cap-policy.md) | Group capping: cap by threads, size groups from wiggum, enforce in CI | Accepted |
| [0034](0034-scaling-regression-suite.md) | Scaling regression suite: gate on speedup ratios, refuse mismatched fixtures | Accepted |
| [0035](0035-behaviour-tree-node-set.md) | Ten composites and decorators, stateless shared leaves, allocation-free ticks | Accepted |
| [0036](0036-utility-scoring.md) | Utility scoring beside trees: curves, inhibition, count compensation, explain | Accepted |
| [0037](0037-dialog-graphs.md) | Dialog graphs are validated before they are played | Accepted |
| [0038](0038-barks-are-budgeted.md) | Barks are a bounded, cooldown-gated channel with a per-frame cap | Accepted |
| [0039](0039-world-director.md) | The director decides, the board applies: policies as values, decisions traced | Accepted |
| [0040](0040-optional-integrations-are-ports.md) | Physics, networking and rendering are ports, not dependencies | Accepted |
| [0041](0041-memory-in-cognition.md) | Memory reaches a tree as three integers; enrolment by proximity and budget | Accepted |
| [0042](0042-diagnostics-and-fuzzing.md) | Explain on demand, overlay as data, decoders fuzzed on two layers | Accepted |
| [0043](0043-docs-and-release-shape.md) | Documentation and release shape: book, README verification, release gate | Accepted |
| [0044](0044-threaded-steering.md) | One frame, cut into slices: threads beat lanes, 2.0x at 10k and 2.8x at 100k | Accepted |
| [0045](0045-reciprocal-local-avoidance.md) | Reciprocal avoidance: half the responsibility each, +1.0 ms at 10k | Accepted |
| [0046](0046-obstacle-driven-navigation.md) | Obstacle navigation rebuilds in rows and publishes once | Accepted |
| [0047](0047-stimulus-perception.md) | Stimuli are a bounded, decaying buffer observers read | Accepted |
| [0048](0048-navigation-links-and-navmesh.md) | Links are search edges; the navmesh is an optional polygon source | Accepted |
| [0049](0049-row-major-neighbour-broadcast.md) | Row-major neighbour broadcast; the lint spine cost 19% until the bound left the loop | Accepted |
