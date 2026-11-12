---
title: Memory and grudges
description: Optional social memory, what it costs, and when not to turn it on.
---

By default an NPC reacts to what it perceives this tick and forgets it immediately. That default is correct for most
crowds: memory is the most expensive thing you can add per agent.

```toml
[dependencies]
bevy-flock = { version = "1", features = ["memory"] }
```

## What changes when it is on

An NPC can hold a small, fixed number of remembered subjects, each with an opinion that decays. Decisions can consult
memory as well as perception, which is the difference between a crowd that flinches and a crowd that holds a grudge.

| property | without `memory` | with `memory` |
| --- | --- | --- |
| per-NPC storage | none | fixed slots, bounded |
| per-tick work | perception only | perception plus ageing, inside the same budget |
| anchors | exempt from folding | exempt from folding and from ageing |
| save size | smaller | grows with remembered subjects |

Ageing runs on the virtual cost scheduler, so memory work is inside the budget you already set rather than added on top
of it. Anchors, the NPCs you pin as story-critical, never forget and never fold, so the character you gave a name is
still holding the grudge when the player returns.

## Do not enable it when

- The crowd never interacts with the player one-to-one. Perception is enough.
- The server simulates more NPCs than its budget covers. Memory multiplies per-agent cost and the multiplication is not
  small.
- You need an audit trail. This is a simulation memory, not an event log. If you need history, record it yourself from
  the events the crate emits.

## Reading the numbers

Per-agent footprint and allocation behaviour are in `docs/dev/tuning/memory.md`, measured by
`examples/memory_probe.rs`. The decision records are `docs/dev/adr/0011-*` (schedule integration), `0012-*`
(persistence) and `0013-*` (subject registry).
