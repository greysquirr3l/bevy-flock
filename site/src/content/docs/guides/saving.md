---
title: Saving a crowd
description: What serialises, what does not, and how to keep a save loadable across versions.
---

A crowd is state: positions, velocities, needs, temperaments, memory rows, the clock. The `serde` feature makes that
state serialisable, and the format carries a version so a save written by an older build is either loaded correctly or
rejected loudly, never silently misread.

```toml
[dependencies]
bevy-flock = { version = "0.1", features = ["serde"] }
```

## What is in a save

- the tick number and the seed material the run started from
- per-NPC state: identity, transform, velocity, needs, temperament
- social memory rows, if the `memory` feature is on
- the subject registry, which maps remembered identities to rows

The registry is why v2 exists: v1 stored attributes positionally, so inserting a new attribute silently shifted every
row's meaning. v2 keys by subject, so an unknown subject is rejected with an error rather than read as the wrong thing.

## Rules that keep saves loadable

1. **Treat the format as an API.** Adding a field is fine; reordering or repurposing one is a version bump.
2. **Keep the seed.** A save plus the seed can reproduce the run; a save without it can only resume it.
3. **Do not save pointers or handles.** Entity ids are not stable across sessions; the registry is what makes memory
   survive a reload.
4. **Test the round trip in CI**, including loading a fixture written by the previous release. There is a compatibility
   suite for exactly this.

## Where to look

- `docs/dev/adr/0012-*`, persistence and format
- `docs/dev/adr/0013-*`, the subject registry
- `docs/dev/adr/0014-*`, the compatibility testing framework
