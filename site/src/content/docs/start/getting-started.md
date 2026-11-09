---
title: Install
description: Add bevy-flock to a Bevy 0.20 project and confirm it runs.
---

## Requirements

- Rust **1.97.1** or newer. The MSRV is a promise, not a suggestion: CI builds it.
- Bevy **0.20**. The crate tracks Bevy's minor versions; a Bevy upgrade is a `bevy-flock` upgrade.

## Add the dependency

```sh
cargo add bevy-flock
```

That is the default feature set: the wide steering kernel, the task-pool path, and everything a normal Bevy game needs.
The [feature flags](/bevy-flock/reference/features/) page covers the rest, including what to turn off for a headless
server.

## Confirm it works

```sh
cargo run --example hello_flock
```

The example is deliberately headless, no window, no GPU, no assets, and prints what the simulation produced:

```text
bevy-flock 0.1.0
  tick            120
  population      2000
  moving          2000
  fastest         1.84 m/s
```

If `moving` is equal to `population` after two seconds, steering is running. If `population` is zero, the preset's spawn
volume did not intersect your grid, see [Concepts](/bevy-flock/start/concepts/#the-grid).

## What to add

Four crates, three of them optional. The first row is the one almost everybody needs.

| I want | add this |
| --- | --- |
| a crowd in a Bevy game | `bevy-flock` |
| all of it, without reading a feature table | `bevy-flock = { version = "0.1", features = ["full"] }` |
| NPCs that walk, gesture and look with skeletons | `bevy-flock` with `features = ["anim"]` |
| a server, replay tool or test harness with no Bevy in the graph | `bevy-flock-core` alone |

`full` turns on `ecs`, `simd`, `threads`, `memory`, `serde`, `navmesh` and `anim`. The animation layer is its own crate
(`bevy-flock-anim`) because it is the only part of the graph that pulls `bevy_animation` and `bevy_asset`, but you never
depend on it directly: `features = ["anim"]` is the switch, and the facade re-exports it at `bevy_flock::anim`.

There are four crates in the repository and you depend on one of them:

| crate | what it is | when you name it |
| --- | --- | --- |
| `bevy-flock` | the facade: re-exports plus the prelude | always |
| `bevy-flock-ecs` | the plugin, components and systems | only if you want the ECS half without the facade |
| `bevy-flock-anim` | clip blending, gaze, gestures, root motion | only if you want the animation without the crowd |
| `bevy-flock-core` | the simulation, with no engine in its graph | only for a headless server or a custom driver |

| crate | engine in the graph | what it is |
| --- | --- | --- |
| `bevy-flock` | Bevy | the plugin, components, systems and the re-exported simulation |
| `bevy-flock-core` | none | the same simulation without an engine, for servers and replays |
| `bevy-flock-anim` | `bevy-flock-ecs` plus `bevy_animation` | where the animation layer lives; a game reaches it through `features = ["anim"]` |

`bevy-flock` re-exports the core, so `bevy_flock::domain::…` and `bevy_flock::behavior::…` resolve whichever you depend
on. Name `bevy-flock-core` directly only when you want no Bevy in your dependency graph at all.

## The whole path in one example

```sh
cargo run --release --example everything --features memory,serde
```

That example is the adoption path in order: add the plugin, ask for a crowd, run frames, read the counts the engine
keeps, then save a memory snapshot and load it back. Read it before the guides; it is eighty lines and it does not
render.
