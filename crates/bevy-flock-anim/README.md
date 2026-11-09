# bevy-flock-anim

**This package is a compatibility shim.** The animation layer it used to contain now lives in
[`bevy-flock`](https://crates.io/crates/bevy-flock) behind the `anim` feature.

```toml
[dependencies]
bevy-flock = { version = "0.1", features = ["anim"] }
```

```rust
use bevy_flock::prelude::*;

app.add_plugins((FlockPlugin::default(), AnimPlugin));
```

Why the split was inverted: `bevy-flock-anim` depended on `bevy-flock`, so `bevy-flock` could not
offer the layer as a feature without a dependency cycle. Anything a game needs to reach should be
reachable from the one crate a game depends on, which is now the case. See
[ADR 0061](https://github.com/greysquirr3l/bevy-flock/blob/main/docs/dev/adr/0061-animation-layer-inversion.md).

Existing code keeps working, because this crate re-exports every item the old one exported. It is
published for the 0.x line and retires at 1.0.
