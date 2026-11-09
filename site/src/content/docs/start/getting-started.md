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

The example is deliberately headless — no window, no GPU, no assets — and prints what the simulation produced:

```text
bevy-flock 0.1.0
  tick            120
  population      2000
  moving          2000
  fastest         1.84 m/s
```

If `moving` is equal to `population` after two seconds, steering is running. If `population` is zero, the preset's spawn
volume did not intersect your grid — see [Concepts](/bevy-flock/start/concepts/#the-grid).

## Two crates, one import

Since the phase-6 split there are two packages:

| crate | engine | use it for |
| --- | --- | --- |
| `bevy-flock` | Bevy | anything with an `App` — the normal case |
| `bevy-flock-core` | none | headless servers, replay tools, CI harnesses, tests |

`bevy-flock` re-exports everything from the core, so `bevy_flock::domain::…` and `bevy_flock::behavior::…` resolve in
both. You only name `bevy-flock-core` directly if you want no Bevy in your dependency graph at all.
