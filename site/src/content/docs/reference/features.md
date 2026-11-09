---
title: Feature flags
description: Every feature, what it turns on, and what it costs when you turn it off.
---

Defaults are what a Bevy game wants. The rest exist so a headless server, a replay tool or a wasm build can take only
what it needs.

| feature | default | what it does | off means |
| --- | --- | --- | --- |
| `simd` | on | four agents at a time through `wide`'s 128-bit lanes | scalar kernel; ~19–31% slower steering at 100k, parity above that |
| `threads` | on | spatial build and steering on the task pool | single-threaded, dependency-light; useful for deterministic replays and small crowds |
| `ecs` | on | the three `Resource` derives and the Bevy-facing half | not for games: this is what makes `bevy-flock-core` engine-free |
| `memory` | off | social memory store and its ageing | no per-agent memory cost at all |
| `serde` | off | save/load and authored tree serialisation | no `serde` or `ron` in your graph |
| `navmesh` | off | polygon navmesh with derived adjacency and string-pulled routes | pathfinding is yours |
| `full` | off | `ecs` + `simd` + `threads` + `memory` + `serde` + `navmesh`, for a game that wants the whole engine | the six defaults above, unchanged |

```toml
# a headless replay harness: no engine, no allocator churn, no tasks
bevy-flock-core = { version = "0.1", default-features = false }
```

## Picking a set

- **A normal game:** defaults. Add `memory` if NPCs should remember players, `serde` if you save.
- **A dedicated server:** defaults plus `serde`; `memory` only if the server is authoritative for relationships.
- **A replay/CI harness:** `bevy-flock-core` with `default-features = false` and `threads` off, one thread, one answer,
  every time.
- **wasm:** turn `simd` off. `wide` targets SSE2/NEON; there is no wasm-simd path, so the scalar kernel is the correct
  choice there.

## MSRV and profiles

`rust-version = "1.97.1"`, built in CI. Debug builds of any crowd simulation are slow enough to mislead you, profile with
`--release`, or set `[profile.dev.package."*"] opt-level = 2` in your own manifest if you need debug builds to be usable.
