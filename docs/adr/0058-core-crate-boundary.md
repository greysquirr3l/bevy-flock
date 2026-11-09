# 0058 — The simulation core becomes a crate, and the engine keeps the adapter
- **Status:** Accepted
- **Date:** 2026-11-09
- **Supersedes:** the packaging half of [0001](0001-hexagonal-layout.md), which chose one crate with module
  boundaries. The layering it established is unchanged; only the thing that enforces it moves.
## Context
[0001](0001-hexagonal-layout.md) drew the hexagon as modules: `domain` and `ports` in the middle, `application`
composing them, `adapters` on the outside talking to Bevy. Since then that has held because people respected it,
and `tests/core_boundary.rs` has now made it checkable — on its first run it found `behavior/social.rs` importing
`crate::adapters::primary::perception` for two blackboard keys, a core behaviour learning its own inputs from an
adapter.

The layout is in better shape than the packaging suggests. Measured at the commit this ADR was written on:

| layer | `bevy_ecs` | `bevy_math` | `bevy_tasks` | `bevy_app` | `bevy_transform` |
| --- | ---: | ---: | ---: | ---: | ---: |
| `ports` | 0 | 0 | 0 | 0 | 0 |
| `domain` | 1 | 10 | 0 | 0 | 0 |
| `behavior` | 1 | 2 | 0 | 0 | 0 |
| `application` | 1 | 2 | 1 | 0 | 0 |
| `adapters` | 20 | 14 | 0 | 16 | 14 |

Files naming the crate, not occurrences. The three `bevy_ecs` entries in the core are two `#[derive(Resource)]`
attributes and one `use bevy_ecs::prelude::*` in `application/mod.rs`; `ports` has no dependencies at all. So the
core is engine-independent except for three lines, and `src/adapters` — twenty-three files — is the only place that
wants an app runtime, a transform hierarchy and a renderer.

The cost of that being a *convention* rather than a boundary is paid by every consumer:

- `cargo tree -p bevy-flock` reports 186 nodes. A server or a headless test harness that wants the crowd simulation
  compiles the engine's app runtime, and every future adapter dependency is a dependency of the simulation too.
- A consumer cannot write their own adapters against a core that has no ECS in its manifest, because there is no
  separate core to depend on. They take the whole crate or nothing.
- The guard is a tripwire, not a wall. It fails a build when a core file *names* `bevy_app`; it cannot stop a
  future contributor from adding a dependency to the manifest and using it, because one crate's dependencies are
  available to every module in it.

## Decision
**Two crates, split along the line the modules already draw, with the public surface held perfectly still.**

- `bevy-flock-core` — `domain`, `ports`, `behavior`, `application`, `error`, `testkit`. Dependencies:
  `bevy_math`, `bevy_tasks`, `smallvec`, `thiserror`, `libm`, plus optional `serde`, `ron` and `wide`. No
  `bevy_ecs`, no `bevy_app`, no `bevy_transform`, no renderer.
- `bevy-flock` — `adapters`, `boids`, `prelude`, the plugin and the crate-level re-exports. Depends on
  `bevy-flock-core` with an **exact** version pin, plus `bevy_ecs`, `bevy_app`, `bevy_transform`.
- `bevy-flock-anim` is untouched. It is already separate for a reason that still holds: it needs
  `bevy_animation`, which no headless simulation should compile.

`boids` moves to the facade despite living at `src/boids.rs` today, because it is nine engine references and two
ECS systems — `FlockSpatialGrid` and the rebuild that drives it. Its public path is unchanged.

### Module paths inside the core do not change

`crate::domain::num` stays `crate::domain::num`. This is the decision that makes the rest of the phase mechanical:
62 files reference `crate::domain`, 20 `crate::behavior`, 17 `crate::application`, and none of them are edited by
the move. The four layers move directory-for-directory, so reviewing a moved file is reviewing a move rather than
a rewrite, and `git log --follow` still finds the history.

### The facade re-exports, it does not re-implement

```rust
pub use bevy_flock_core::{application, behavior, domain, error, ports, testkit};
```

`bevy_flock::domain::num::f32_to_i64` — the path six modules of the software demo import, and the one the
conversion-helper docs point at — resolves unchanged, as do `::application`, `::behavior`, `::ports`, `::testkit`
and `::error`. The prelude keeps its current
contents, including the `bevy_app::App` / `bevy_transform::Transform` re-exports, because it belongs to the facade
and a game that uses the plugin needs exactly those.

The acceptance test for this is structural rather than stylistic: **the root test suite must pass with zero test
file edits.** A test that had to change its import is evidence the guarantee was broken, and the guarantee is the
task.

### Feature flags forward by name

| feature | core | facade | why |
| --- | --- | --- | --- |
| `default` | `simd`, `threads` | same | the facade's default must mean what it means today |
| `simd` | `dep:wide` | forwards | the crowd and band kernels are core code |
| `threads` | `bevy_tasks/multi_threaded` | forwards | slicing is core; the ECS-side use comes free |
| `memory` | none | forwards | social memory is a domain store |
| `serde` | `dep:serde`, `dep:ron`, `smallvec/serde` | forwards | blueprints and snapshots are domain types |
| `navmesh` | none | forwards | the largest single domain module, still domain |

Two rules, because this is where the split fails quietly:

1. A facade feature forwards to a core feature of the same name and adds only what the adapter itself needs. A
   facade feature that compiles the adapter's half of a capability while the core's half stays off produces a
   build that links but does not work.
2. `default` is identical in both crates. A consumer writing `default-features = false` is relying on a specific
   meaning, and a split is not licence to change it.

### One core, exactly

Two versions of `bevy-flock-core` in one binary is a compile error at the point of mixing, not a silent wrong
answer — but it is a confusing compile error, and the way to avoid ever seeing it is the exact pin
(`bevy-flock-core = { version = "=0.1.1", ... }`) plus publishing both crates at the same version from the same
commit. `multiple_crate_versions` is one of the six allows in the lint policy; this is the one place it would
actually cost something, so it is handled by discipline in the manifest rather than by the lint.

## Consequences
- A headless consumer adds `bevy-flock-core` and gets a simulation whose dependency set is a vector type, a task
  pool, `smallvec`, `thiserror` and `libm`. The number of nodes it saves is measured by the phase's last task and
  recorded in `docs/performance.md`; this ADR deliberately does not guess it.
- `cargo test -p bevy-flock-core` becomes a meaningful thing to run — the simulation suite, without the app
  runtime. `cargo test` at the root still builds everything, because the adapters need the core.
- A new manifest means a new strict lint spine and a new entry in `scripts/lint_audit.py`'s manifest list. That is
  a real cost of the split and the reason it is two crates rather than five.
- `tests/core_boundary.rs` stays. With the ECS ledger emptied it loses its `bevy_ecs` test and keeps the other
  three: the compiler now enforces the dependency boundary, but nothing but the guard enforces "core must not
  depend on adapters" *within* a crate, and the facade still contains both.
- The intra-doc links from adapters into core types (`[SpatialGrid]`, `[Locomotion]`) resolve through the
  dependency as they do now; `cargo doc` for the facade is the check, and docs.rs builds each crate separately.
- If the split turns out to buy nothing measurable for a headless consumer, that is reported rather than buried:
  the boundary is still worth having, but the case for it would be the compiler's rather than the clock's, and the
  performance record would say so.

## Rejected alternatives
- **Five crates — `domain`, `ports`, `behavior`, `application` as separate packages.** No consumer can observe the
  difference: Cargo builds a dependency chain in order regardless, so only the root of the chain gets faster, and
  every internal type would need a re-export layer at each hop. Four more manifests, four more lint tables, for a
  boundary the module system already draws.
- **Keep one crate and rely on `tests/core_boundary.rs`.** The guard checks what a file *names*. It cannot stop the
  manifest from growing `bevy_render` and a new module from using it, and it says nothing to a consumer who wants
  the simulation without the engine. A tripwire is worth having; it is not a wall.
- **Put the ECS behind a core feature that is on by default.** This keeps `bevy_ecs` in the core's manifest, so the
  core still compiles it for a consumer who forgets the flag, and the "headless build" becomes a configuration
  rather than a guarantee. The phase deliberately moves the three derives into the adapter layer instead.
- **A `bevy-flock-types` crate for the shared vocabulary.** Tempting until you notice that the vocabulary is the
  domain, and a types crate would either depend on the domain (circular) or duplicate it.
- **Fold `demo`, `demo-gpu` and `playground` into the workspace.** They are excluded on purpose, each with its own
  lock file, release profile and reason to exist; a root `cargo test` would build three renderers before it ran one
  simulation test. The split does not change that calculus.

## Boundary audit against the post-phase-5 tree

The decision above was written before phases 2-5 landed. Audited against the tree as it stands, with `tests/core_boundary.rs`
green (4 tests) and every `use bevy_*` in `domain`, `behavior` and `application` enumerated:

| engine crate | imports in the core layers | verdict |
| --- | ---: | --- |
| `bevy_math` | 14 | fine — it is a maths crate (`Vec3`, `Mat3`), not engine runtime |
| `bevy_tasks` | 1 | fine — a thread-pool abstraction the core needs, not a Bevy app |
| `bevy_ecs` | **3** | **drift: these must be rehomed** |

The three ECS touch points are narrow and all of them are `Resource` derives, not systems:

- `domain/memory.rs:506` — `#[cfg_attr(feature = "memory", derive(bevy_ecs::prelude::Resource))]`
- `behavior/registry.rs:144` — `#[derive(Default, bevy_ecs::prelude::Resource)]`
- `application/mod.rs:22` — `use bevy_ecs::prelude::*;` (the systems module, which belongs to the facade regardless)

So the boundary as decided is *almost* true already: the core's logic has no engine dependency, and its data types carry one
derive each. This is why `extract-core-crate` moves directories rather than rewriting them, and why `rehome-ecs-resources`
is the task immediately after it rather than an afterthought — the three derives are the only thing standing between the core
and `cargo build -p bevy-flock-core` with no Bevy app in the graph.

Two modules added by phases 2-5 were not in the original module list and are confirmed inside the boundary rather than
outside it: `domain/continuum.rs` and `domain/cognition.rs`, both engine-free.

### Why the derives are gated rather than moved

The audit above found three `bevy_ecs` mentions in the core layers, all `Resource` derives. Three mechanisms were
tried against the real compiler, and only one survives:

1. **A hand-written `impl Resource` in the adapter.** Fails. In Bevy 0.20 `Resource` is `trait Resource: Component {}`,
   so a resource needs *both* impls — and even with a hand-written `Component` (`STORAGE_TYPE = Table`,
   `Mutability = Mutable`), `App::insert_resource` followed by a `Res<SimClock>` parameter fails system validation with
   "Resource does not exist". Whatever `#[derive(Resource)]` registers is not reproducible from the public surface.
2. **Newtype wrappers in the adapter** (`struct Clock(SimClock)`). Works, but moves every system signature in the
   adapter from `Res<SimClock>` to `Res<Clock>` and every `&store` passed to a domain function to `&*store`: nine files
   and roughly 170 mechanical edits, for a boundary that is about which crate *names* the trait rather than which type a
   system borrows.
3. **Gate the derive behind a feature.** Adopted. Each type carries
   `#[cfg_attr(feature = "ecs", derive(bevy_ecs::prelude::Resource))]`, the facade enables `ecs` by default, and the
   core crate will depend on the facade with `default-features = false` and get a graph with no `bevy_ecs` in it.

The gate is enforced rather than asserted: `core_mentions_of_the_engine_are_feature_gated` in `tests/core_boundary.rs`
fails if any `bevy_ecs` token in a core file is not on a line naming `feature = "ecs"`. A bare derive would compile,
pass the ledger check, and silently put the engine back in the core's graph — so the ledger alone was not enough.

`advance_clock` moved with the decision: it is a system, not a value, so it now lives in `src/adapters/primary/clock.rs`
and `application` has no `bevy_ecs` type in any signature.

**Still open for T02:** `--no-default-features` does not yet build, because the adapter modules themselves need the engine.
Gating `pub mod adapters` on the same `ecs` feature is what turns the facade into a crate that can be depended on as a
core-only library, and it is the last thing standing between the boundary as documented and the boundary as built.
