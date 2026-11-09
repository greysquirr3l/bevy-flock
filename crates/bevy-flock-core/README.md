# bevy-flock-core

The simulation half of [`bevy-flock`](https://crates.io/crates/bevy-flock): a spatial index, steering,
behaviour trees, budgeted cognition, social memory and the continuum ambient tier, with **no Bevy app in
the dependency graph**.

This is the crate to depend on when there is no engine in the process: a headless server that simulates a
city, a replay harness that has to be deterministic, a test that wants a million agents without a window.

```toml
[dependencies]
bevy-flock-core = "0.1"
```

```rust
use bevy_flock_core::domain::spatial::{SpatialGrid, SpatialGridConfig};
use bevy_flock_core::domain::steering::SteeringParams;

let grid = SpatialGrid::new(SpatialGridConfig::default());
# let _ = grid;
```

Determinism is the contract, not an aspiration: every float that decides a position goes through
`domain::num`, which exists so the same seed produces the same crowd on every platform. The parity tests
that hold that line live in the facade, where the scalar, SIMD and threaded roads are compared bit for bit.

Most games should depend on `bevy-flock` instead: it re-exports this crate at `bevy_flock::domain`,
`bevy_flock::behavior` and `bevy_flock::application`, and adds the plugin on top. See
[ADR 0062](https://github.com/greysquirr3l/bevy-flock/blob/main/docs/dev/adr/0062-three-crate-layering.md)
for how the four crates fit together.
