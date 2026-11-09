# ADR 0061: The animation layer inverts into a feature

**Status:** accepted
**Date:** 2026-11-09
**Supersedes part of:** ADR 0052 (which placed the binding layer in its own crate)

## Context

`bevy-flock-anim` depended on `bevy-flock`. That direction made three things impossible:

- `features = ["full"]` could not include the animation layer, because a feature of a package cannot
  add a dependency on a crate that depends back on that package. Cargo rejects the cycle.
- The facade could not re-export the layer, so the answer to "how do I make the crowd walk" was a
  second package name that nothing in the primary crate mentioned. Adoption is the thing this
  project is judged on, and that is an adoption defect, not a packaging preference.
- A game that wanted both had to keep two versions in step by hand, and the facade's own docs had to
  explain a split that served the dependency graph rather than the reader.

The original reason for the split was real: the simulation core must not pull `bevy_animation` or
`bevy_asset`, and a dedicated server should never compile them. That reason survives the inversion
only if the layer is a feature that is off by default.

## Decision

The animation layer moves into the facade at `src/anim`, behind the `anim` feature:

```toml
anim = ["ecs", "dep:bevy_animation", "dep:bevy_asset", "dep:bevy_reflect"]
full = ["ecs", "simd", "threads", "memory", "serde", "navmesh", "anim"]
```

`bevy-flock-anim` becomes a four-line re-export shim so an existing `use bevy_flock_anim::AnimPlugin`
keeps compiling. It carries no logic, no tests and no dependency beyond `bevy-flock` with the
feature on. It is retired at 1.0, not at 0.2: removing a path that already compiles in somebody's
game is a semver break, and not one worth taking for tidiness.

The alternative that looked cleanest on paper was a third crate between core and facade
(`bevy-flock-ecs` holding the adapters, with `bevy-flock-core` below and the animation layer beside
it). It was rejected because it moves twenty modules to make a dependency graph prettier while
changing nothing a consumer sees, and because the animation layer is genuinely a Bevy-facing
adapter: the facade is exactly where Bevy-facing adapters belong.

Two other options were rejected outright:

- **Move the component types the layer needs into `bevy-flock-core`.** Components are engine types.
  ADR 0058 draws the core boundary at "no engine", and `Locomotion`, `GazeAim`, `NpcGestures`,
  `NpcTier` and `NpcBlackboard` are all ECS-facing. Putting them in core to satisfy a packaging
  problem would corrupt the boundary the project spent a phase establishing.
- **Make the layer generic over component types.** Bevy queries are structural: a generic system
  parameterised by a "component bundle" forces every game to instantiate the generics and read the
  instantiation before their first crowd walks. It also makes the layer's own tests harder to write
  than the layer.

## Consequences

- One dependency gets a crowd that walks: `features = ["full"]`, or `features = ["anim"]`.
- A dedicated server still never compiles `bevy_animation` or `bevy_asset`, because the feature that
  needs them is off by default and `--no-default-features` was verified warning-free after the move.
- The facade gains three optional dependencies. That is the price of the property, and it is paid
  only by builds that ask for the layer.
- The animation bench, its baseline, its no-allocation test and its two examples move to the facade,
  because they test code that now lives there. `benches/anim-baseline.json` is the file the CI gate
  reads; the old path under `crates/bevy-flock-anim` no longer exists.
- Two dead-code sites in the core surfaced while checking the new feature combination
  (`block_bases` and `prefix_bases` on the no-threads path, and an orphaned `build_parallel` stub).
  They are now gated or gone rather than left to warn in a configuration nobody built before.
- The shim crate's lint table is the facade's table. A shim that was allowed to weaken the policy
  would be a hole in the audit, and `scripts/lint_audit.py` caught exactly that during the move.

## Verification

- `cargo check -p bevy-flock --no-default-features`, `--features anim`, and `--all-features` all
  build with zero warnings.
- `cargo check --all-targets --all-features` covers the moved tests, benches and examples.
- The playground depends on the feature instead of the crate, and builds.
- `scripts/lint_audit.py` reports no unsanctioned override; the module-level `suboptimal_flops`
  expectation moved with the file and is listed at its new path.
