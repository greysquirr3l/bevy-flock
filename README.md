# bevy-flock

<img src="docs/demo/bevy-flock-logo.png" width="320" height="320" align="right" alt="bevy-flock: a ring of ink-wash cranes">

**Deterministic crowd and NPC simulation for Bevy.** One NPC or a million, with
pluggable cognition and a replay you can trust.

```rust
use bevy_app::App;
use bevy_flock::prelude::*;

let mut app = App::new();
app.add_plugins(FlockPlugin::new(FlockConfig::preset(CrowdPreset::CityAmbient)));
app.update();
```

That is a running crowd of 2,000 people, each with their own personality, their
own random stream, and a spatial index built under them. No assets, no window, no
GPU.

**Documentation:** [Read the bevy-flock docs](https://greysquirr3l.github.io/bevy-flock/) ·
**Internals:** [`docs/dev/`](docs/dev) · **Book:** [`docs/book/`](docs/book) ·
**Crates:** [`bevy-flock`](https://crates.io/crates/bevy-flock),
[`bevy-flock-core`](https://crates.io/crates/bevy-flock-core)

---

## What you get

**Steering that looks like a crowd.** Separation, alignment, cohesion, seek, arrive, and
reciprocal local avoidance with per-NPC priority, so a big guard walks through a crowd rather
than around it.

**Personality that changes the walk.** Temperament traits bias steering weights and need decay,
which is why two NPCs in the same crowd do not move the same way. Needs drain, priorities shift,
and the crowd develops a mood without anyone scripting it.

**Cognition where it is affordable.** Distance decides how much thinking an NPC gets: reflex
steering far away, a cheap decision in the middle distance, full behaviour trees with memory and
dialogue near the player. The budget is virtual cost per tick, so an expensive archetype costs
more of it than a cheap one.

**Behaviour you can plug in.** Behaviour trees with compiled flat nodes, resume, abort, cooldown,
timeout and parallel; state machines where a tree is the wrong shape; and a registry that lets
you add your own leaves with your own costs. Per-NPC, per-group or per-population.

**Memory that is optional.** NPCs can hold a few remembered subjects with opinions that decay,
which is the difference between a crowd that flinches and a crowd that holds a grudge. It is
feature-gated because it is the most expensive thing you can add per agent.

**A million when you need them.** Above roughly 250,000 inhabitants, distant NPCs become a
density and velocity field that manifests individuals as the camera approaches and folds them
back as it leaves. Anchors never fold; population is conserved exactly.

**Determinism you can test against.** Same seed, same features, same platform gives the same
state digest over hundreds of ticks. Saves carry a version; v1 files still load.

**Neighbours without the O(n²).** A uniform spatial grid, rebuilt per frame or every N frames,
with radius and k-nearest queries. Its parallel and serial builds are byte-identical, worker
count changes speed, never output.

Not included, on purpose: rendering, physics, networking, asset loading, dialogue content. This
crate simulates a crowd; Bevy does the rest. The [coverage page](https://greysquirr3l.github.io/bevy-flock/reference/coverage/)
is explicit about where the line is.

## Install

```sh
cargo add bevy-flock
```

Bevy 0.20, Rust 1.97.1. Defaults are what a game wants: the wide steering kernel and the
task-pool path. `memory`, `serde` and `navmesh` are opt-in; `bevy-flock-core` is the same
simulation with no engine in its dependency graph, for servers and replays. The
[feature page](https://greysquirr3l.github.io/bevy-flock/reference/features/) has the table.

## Verify it in ten minutes

```sh
cargo run --example hello_flock          # a crowd, headless, no assets
cargo test --workspace                   # the whole suite
cargo run --release --example profile_crowd
```

`hello_flock` prints the tick, the population, how many are moving and the fastest speed. If the
population is zero, the preset's spawn volume missed your grid, the
[concepts page](https://greysquirr3l.github.io/bevy-flock/start/concepts/) explains the grid.

## Performance

Reference machine: shared cloud vCPU, x86_64, `--release`. Ratios and shape, not a hardware
claim, measure your own before quoting these.

| | measured | gate |
| --- | --- | --- |
| 1,000,000-inhabitant band frame, wide kernel | 3.78 ms | < 6 ms |
| 1,000,000-inhabitant band frame, scalar | 4.75 ms | < 6 ms |
| 100,000 with 5,000 manifested | 3.37 ms | < 8 ms |
| 1,000,000-body spatial rebuild, 6 workers | 4.572 ms | |
| Per agent | 32 B wide / 28 B scalar | |
| Steady-state allocations over 60 frames | 0 | 0 |

Every number, including the optimisations that were measured and rejected, is in
[`docs/dev/tuning/performance.md`](docs/dev/tuning/performance.md).

## What to add

| I want | add this |
| --- | --- |
| a crowd in a Bevy game | `bevy-flock` |
| all of it | `bevy-flock = { version = "0.1", features = ["full"] }` |
| NPCs that walk with skeletons | `bevy-flock` with `features = ["anim"]` |
| no Bevy in the dependency graph | `bevy-flock-core` alone |

`full` is `ecs` + `simd` + `threads` + `memory` + `serde` + `navmesh` + `anim`. The animation layer is a feature rather
than a crate so that one dependency gets a crowd that walks; it is off by default because it is the only feature that pulls
`bevy_animation` and `bevy_asset`, which a dedicated server should never compile.

## Where the documentation lives

- **The site**, [getting started, guides, and reference](https://greysquirr3l.github.io/bevy-flock/):
  install, first crowd, concepts, guides, reference. Start here.
- **The book**, [`docs/book/`](docs/book), deeper and older, built with mdBook.
- **`docs/dev/`**, ADRs, tuning journals, gates, benchmark harnesses. Written for whoever
  changes the engine, kept in the repository and out of the crates.io tarball on purpose.

## MSRV

Rust 1.97.1, pinned in `rust-toolchain.toml` and built in CI.

## Licence

MIT OR Apache-2.0, at your option.
