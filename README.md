# bevy-flock

**Deterministic crowd and NPC simulation for Bevy.** One NPC or a million, with
pluggable cognition and a replay you can trust.

```rust
use bevy_app::App;
use bevy_flock::prelude::*;

let mut app = App::new();
app.add_plugins(FlockPlugin::new(FlockConfig::preset(CrowdPreset::CityAmbient)));
app.update();
```

That is a running crowd of 2,000 people, each with their own personality,
their own random stream, and a spatial index built under them. No assets, no
window, no GPU.

---

## Status

`0.1.0` is the simulation foundation, and it is deliberately honest about what
that means:

| Area | State |
| --- | --- |
| Boids and steering (separation, alignment, cohesion, seek, arrive) | **Shipped** |
| Deterministic spatial grid (radius, k-nearest) | **Shipped** |
| Deterministic RNG with per-NPC streams (PCG32, reference-verified) | **Shipped** |
| Budgeted, deterministic population spawning (unique and recycled) | **Shipped** |
| Behaviour trees: flat compiled nodes, resume, abort, cooldown, timeout, parallel | **Shipped** |
| Leaf catalogue with declared costs and per-archetype trees | **Shipped** |
| Social memory: bounded slot store, cold archive, lazy decay (`memory` feature) | **Shipped (opt-in)** |
| Versioned save format with a subject registry and a committed fixture corpus | **Shipped (opt-in)** |
| Diagnostics: ghost rows, orphan slots, SVG graphs (`registry_telemetry` example) | **Shipped (opt-in)** |
| Guarded state machines with dwell times | **Shipped** |
| Extension registry for game-defined logic | **Shipped** |
| Personality, needs and mood model | **Shipped** |
| Virtual-cost fair-queue think scheduler (ADR 0009) | **Shipped** |
| Proximity weights with hysteresis, wired to the scheduler | **Shipped** |
| Tiered cognition: reflex, reactive, deliberative, social (ADR 0017) | **Shipped** |
| Ambient continuum: population as density past the manifest band (ADR 0018) | Proposed |
| Continuum ambient tier (millions of inhabitants) | Planned |
| Dialogue graphs, barks, facts | Planned |
| Versioned snapshots and compatibility corpus (ADR 0012-0014) | **Shipped (opt-in)** |
| Render adapters, avian, replicon | Planned |

The full 40-task plan lives in [`plan.toml`](plan.toml), including the
capability contracts and the acceptance scenarios each future task must satisfy.
Anything above marked *Planned* has a specification and a test plan, not an
implementation — treat this table as the source of truth.

## Why another crowd crate

`big-brain` is utility AI for Bevy and last shipped against Bevy 0.15; `bevy_boids`
is a flocking demo. Neither answers the questions a game actually hits at scale:
what does the crowd cost per frame, what happens at 100,000 agents, and can I
replay the frame that crashed?

`bevy-flock` is built around three answers:

1. **Determinism is a feature, not a constraint.** Fixed logical ticks, per-NPC
   RNG streams derived from `(seed, NpcId)`, `libm` instead of `std` transcendentals,
   no hash iteration order anywhere in simulation code. The same seed produces the
   same world on Linux, macOS, Windows and wasm.
2. **Cost is explicit.** Steering has an accelerations limit and a priority
   budget, neighbours come from a spatial index rather than an `O(n²)` scan, and
   the neighbour buffer is a `Local` — a frame that steers 100,000 NPCs allocates
   once, on the first frame, and never again.
3. **The engine knows nothing about rendering.** No `Sprite`, no `Mesh`, no
   camera. A server can run the same crowd with `default-features = false`, and a
   renderer observes the simulation instead of driving it.

## What the code looks like

```rust
use bevy_app::App;
use bevy_flock::prelude::*;

let mut app = App::new();
app.add_plugins(FlockPlugin::new(
    FlockConfig::preset(CrowdPreset::BattleHorde)
        .with_population(PopulationSize::clamped(10_000))
        .with_seed(SimSeed::new(42)),
));
```

Spawning is budgeted, so ten thousand NPCs arrive over a few frames instead of
one 400 ms hitch:

```rust
let config = FlockConfig {
    population: PopulationSize::clamped(100_000),
    spawn_budget: 4_096, // entities per frame
    ..FlockConfig::default()
};
```

Behaviour is data:

```rust
use bevy_flock::prelude::*;

let tree = BehaviorTree::try_from_spec(sequence(vec![
    condition(IS_ALARMED),          // game-defined leaf ids
    selector(vec![action(FLEE), action(HIDE)]),
]))?;
```

An authored tree is validated when it is compiled, not when it is ticked: an
empty composite or a tree deeper than 64 levels is an authoring error, and the
caller decides whether that is fatal.

and logic a game writes is registered once:

```rust
struct Greet;

impl LeafBehavior for Greet {
    fn tick(&mut self, ctx: &mut LeafContext<'_>) -> Status {
        // read ctx.blackboard, write what the next leaf needs
        Status::Success
    }
    fn label(&self) -> &'static str { "greet" }
}

let mut registry = FlockRegistry::new();
let greet = registry.register_leaf("greet", Greet)?;
```

## Determinism, precisely

The guarantee is: *same binary, same platform, same features, same seed and the
same tick count produce the same world state*. That is what the tests assert, by
hashing positions and velocities in `NpcId` order and comparing the result.

It is not a claim that different machines agree when you change the feature set
or swap the physics adapter, and the docs say so rather than implying more.

Three mechanisms make it hold:

- **Logical ticks.** The simulation advances a `u64` counter. Wall-clock time is
  never read in simulation logic, so a 40 ms frame does not skip a tick.
- **Per-entity streams.** `RngStream::for_npc(seed, id)` is a pure function, so
  adding an NPC does not shift anyone else's draws.
- **Ordered queries.** The spatial grid returns `(distance, NpcId)`-ordered
  results from a flat array sorted by `(cell id, insertion index)` — a total
  order, so no hasher, no pointer order and no thread scheduling can change it.

## Benchmarks

### Optional: four agents at a time

`features = ["simd"]` runs the steering kernel four agents wide with `wide::f32x4`, behind a
cell-major block order. It is off by default so the default build has no dependencies.

| Workload | Scalar | `simd` |
| --- | --- | --- |
| 1,000,000 band frame | 4.75 ms | **3.78 ms** |
| packed steer, 50,000 agents | 138 ms | **56.5 ms** |
| packed steer, 5,000 agents | 1.31 ms | **1.20 ms** |

The wide path is bit-identical to the scalar one, which stays in the crate as the reference and
is tested against it on every run. ADR 0023 has the design and the three slower attempts.

Run them yourself:

```sh
cargo bench --bench crowd
```

A headless frame — spawn drain, grid rebuild, steering, integration — on the
6-vCPU ARM64 reference host described in [`docs/performance.md`](docs/performance.md):

| NPCs | Frame | Per agent |
| --- | --- | --- |
| 1 | 1.01 ms | — |
| 1,000 | 1.63 ms | 1.6 µs |
| 10,000 | 7.7 ms | 0.77 µs |
| 100,000 | 73.5 ms | 0.74 µs |

Per-agent cost is flat because neighbour queries are local: a radius-8 m query is
138 ns at 1,000 agents and 552 ns at 100,000. Ten thousand steered NPCs fit a 60 Hz
frame with 2.2× of margin — the ADR 0026 kernel cut that frame from 18.2 ms.

The first version of the spatial index cost **37 ms for a crowd of one**, because
it materialised 15.6 million cells every tick. The second fixed that and lost 4×
at scale. What shipped is the third design, and
[`docs/performance.md`](docs/performance.md) has both losing designs, their
numbers, and why they lost — a benchmark table without the failures is marketing.

### Cost per agent

32 bytes of steering state per NPC (positions, velocities, desired, bucket chain, cell-major
order), no allocation in the steady state, 30.5 MiB for a million. Measured, with cache
profiles, in [`docs/memory.md`](docs/memory.md).

## Architecture

```
src/
  domain/         pure simulation math and state — no Bevy, no clock, no I/O
  application/    the logical clock and orchestration
  adapters/
    primary/      plugin, components, systems — what a game calls
    secondary/    indexes and stores the simulation owns
  behavior/       trees, state machines, blackboard, extension registry
  boids.rs        the ECS systems that drive steering each frame
```

Rendering is not an adapter. It observes; it cannot drive. See
[`ARCHITECTURE.md`](ARCHITECTURE.md) and [`docs/adr/`](docs/adr/) for the
reasoning, including the alternatives that were rejected.

## MSRV

Rust **1.97.1**, which is Bevy 0.20's declared minimum. `glam` is pinned to the
0.33 line that Bevy 0.20 uses — 0.34 exists and will silently give you two
incompatible `Vec3` types, so `cargo tree -d -i glam` should never show two.

## Licence

MIT OR Apache-2.0, at your option.
