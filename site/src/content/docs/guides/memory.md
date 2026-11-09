---
title: Memory and grudges
description: Optional social memory, what it costs, and why it is feature-gated.
---

By default an NPC reacts to what it perceives this tick and forgets it immediately. That is the right default: memory is
the single most expensive thing you can add per agent, and most crowds never need it.

## What the feature buys

With the `memory` feature, an NPC can hold a small number of remembered subjects — the player who robbed it, the guard who
helped it — each with an opinion that decays over time. Decisions can then consult memory as well as perception, which is
the difference between a crowd that flinches and a crowd that holds a grudge.

```toml
[dependencies]
bevy-flock = { version = "0.1", features = ["memory"] }
```

## The cost model

Memory is bounded per NPC: a fixed number of slots, each a small record. It is not a log and it is not unbounded — an NPC
that meets a thousand players remembers the ones that mattered by decay, not by history. Ageing runs on the same virtual
cost scheduler as everything else, so memory work is inside the budget you already set rather than on top of it.

Anchors — NPCs you have pinned as story-critical — are exempt from ageing and from the fold, so the NPC you gave a name
does not forget the player while you are not looking.

## When not to use it

- A crowd that never interacts with the player one-to-one: perception alone is enough.
- A server simulating more NPCs than it has budget for: memory multiplies per-agent cost.
- Anything where you need an audit trail. This is a simulation memory, not an event log; if you need history, record it
  yourself from the events the crate emits.

## Where to look

- `examples/memory_probe.rs` — what the store costs per agent, measured
- `docs/dev/adr/0011-*`, `0012-*`, `0013-*` — the schedule integration, the persistence format, the subject registry
- `docs/dev/tuning/memory.md` — footprint and allocation numbers
