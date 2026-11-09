# ADR 0062: Four crates, one direction, and nothing in `src/` but re-exports

**Status:** accepted
**Date:** 2026-11-09
**Supersedes:** ADR 0061's shim decision (the `bevy-flock-anim` compatibility crate is gone)

## Context

ADR 0058 extracted `bevy-flock-core`. ADR 0061 inverted the animation layer into a facade feature after
`bevy-flock-anim` turned out to depend on `bevy-flock`, which is a cycle the moment a feature has to
offer it. Both were correct as far as they went, and both left the facade as the last mixed crate: 37
files and 14,937 lines, of which 226 were the actual facade. Nine thousand lines of ECS adapters, a
thousand of legacy boids, and four thousand of animation sat in `src/` because they had nowhere else
to be.

That is a packaging compromise with real costs:

- `cargo doc -p bevy-flock` documents a crate whose public surface is mostly another crate's code.
  Every link that points at a facade-only path from inside the core broke, which is how seventeen
  broken links were found.
- A headless consumer cannot depend on the adapters without also getting, in name, the crate that
  advertises a crowd engine. The `--no-default-features` facade is honest but the file layout is not.
- The animation layer had to live in the facade to be a feature, which forced a shim crate to keep
  `bevy_flock_anim::` compiling, which forced an ADR to explain a compatibility path nobody uses.

The project has no consumers yet. That fact is the decision: a compatibility shim is a liability with
no beneficiary, and the layout can be fixed now instead of carried.

## Decision

Four crates, one dependency direction:

```
bevy-flock-core   no engine at all: domain, behavior, application
      ↑
bevy-flock-ecs    the plugin, components, systems, boids    (bevy_app, bevy_ecs, bevy_transform, bevy_tasks)
      ↑
bevy-flock-anim   clip blending, gaze, gestures, root motion (bevy_animation, bevy_asset)
      ↑
bevy-flock        re-exports only: core, and the other two behind `ecs` and `anim`
```

`src/` now holds two files and 226 lines: `lib.rs` (module re-exports and crate docs) and `prelude.rs`
(the names a game imports). Nothing else. `bevy_flock::adapters::primary::…`, `bevy_flock::boids::…`
and `bevy_flock::anim::…` still resolve, because a `pub use` is a path.

The facade owns the prelude and the docs, and nothing else. Its `Cargo.toml` has three dependencies:
the core, and the other two crates behind features. `--no-default-features` gives a simulation library
with no engine in the graph; `features = ["full"]` gives a game the crowd that walks.

`bevy-flock-ecs` re-exports the core's modules at its own root (`domain`, `behavior`, `application`,
`error`), which is why the moved adapter files needed no path edits beyond their own crate. That
re-export is deliberate: it keeps `crate::domain::spatial` correct inside the crate that owns the
adapters, and it gives a consumer of the ECS crate alone the simulation types it needs to call those
systems.

`bevy-flock-anim` depends on `bevy-flock-ecs`, not on the facade. That is the whole reason the shim
could be retired: the layer needs `Locomotion`, `GazeAim`, `NpcGestures`, `NpcTier` and the
blackboard, all of which are ECS types, and it needs them from a crate it can depend on without a
cycle.

## Migration, in the order it had to happen

1. `crates/bevy-flock-ecs` created; `src/adapters` and `src/boids.rs` moved in with `git mv`.
2. The ECS crate gets its own prelude (the names the facade used to assemble by hand) so the crate is
   usable alone.
3. `src/anim` moved to `crates/bevy-flock-anim/src`, replacing the shim.
4. The facade's prelude became two lines of glob plus its core list; its `lib.rs` became re-exports.
5. The animation bench, baseline, no-allocation test and both examples moved back to the anim crate,
   which is where they belong now that it owns the code again.
6. Every manifest's lint table is the same table. `scripts/lint_audit.py` learned the new manifest and
   the two new crate roots, and refused to pass until it did: a crate with a lint table that the audit
   has never heard of is a hole in the policy.

## Consequences

- `cargo doc` now produces four coherent pages, and each one documents its own code. The 17 broken
  links ADR 0061 documented are structurally impossible in this shape: the core only links to itself,
  because that is all it can see.
- The facade's crate-level `clippy::suboptimal_flops` expectation became unfulfilled and was removed.
  Its manifest-level allowance went with it: the arithmetic that needed it lives in the core now.
- Three of the facade's dependencies left with the code: `bevy_app`, `bevy_ecs`, `bevy_transform`,
  `bevy_tasks`, `smallvec` and `libm` are the ECS crate's problem now. What the facade still needs for
  its own tests and examples it takes as dev-dependencies, which never reach a consumer.
- The published facade tarball is still `exclude = ["crates/"]`, so its `path` dependencies are the
  release question: the three crates have to publish together, in order, at the same version. That is
  a release-checklist item, not a code one, and it is written down there.
- `bevy-flock-anim` is a real crate again and has no compatibility story to tell. Anything that
  imported it before this ADR did not exist yet.

## Alternatives rejected

- **Leave the adapters in the facade.** It works, and it is what shipped through 0.1. But the facade
  then documents other crates' code, which is the defect that produced seventeen broken doc links the
  last time it was let through.
- **Keep the shim "just in case".** A re-export crate with no users is a maintenance tax and a false
  signal: it says the project supports a path that no code depends on.
- **Merge the ECS and animation crates.** Tempting, because the anim crate is small, but it would put
  `bevy_animation` and `bevy_asset` in the graph of every server that wants the adapters. The two
  features have different dependency costs, so they are different crates.

## Verification

- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: clean.
- `cargo doc --workspace --all-features` with `-D warnings`: zero errors across four crates.
- `cargo test --workspace --all-features`: 811 tests, 25 suites.
- `cargo check -p bevy-flock --no-default-features`: zero warnings, no engine in the graph.
- `scripts/lint_audit.py`: 186 files scanned, no unsanctioned override, self-test passing.
- `src/` is two files, 226 lines, and both are re-exports.
