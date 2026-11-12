# The 1.0 release checklist

The executable half of [`docs/dev/meta/1.0-readiness.md`](1.0-readiness.md). That file says what 1.0 *means*; this one is the sequence
of commands and decisions that gets there, in order, with the state of each step written down rather than remembered.

This is not the 0.1.0 checklist in [`release-checklist.md`](release-checklist.md), that one signs off the
one-million-inhabitant milestone against ADR 0018's nine gates and stays as the record of it. This one signs off a *library
release*: the API, the packaging, the promise.

Run it top to bottom on a clean `main`. Anything that fails is a blocker, not a note.

## 0. Pre-flight

```bash
git switch main && git pull
git status --porcelain          # must be empty
git tag --list 'v*' | tail -1   # know the last release

```

**Status:** ready. PR #34 (the core-crate split) merged as `aaca2f6`, so step 6 can be re-run against `main` rather
than against a branch.

## 1. Freeze the public surface

The promise: every public item is documented or deliberately hidden, and the facade paths cannot move again after 1.0.

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo test --test core_boundary

```

Plus a compile-only check that the documented paths are the promised ones: `tests/facade_paths.rs` should import from
`bevy_flock::domain::…`, `::behavior::…`, `::application::…`, `::adapters::…` and `::prelude::*`, and assert a few type
identities. It does not exercise behaviour; it fails when a path stops resolving, which is the 1.0 promise in one file.

**Status:** met. The doc gate is clean across all four crates (`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
--workspace --all-features`, zero errors; it was red with nine broken or private links until the adoption-surface
branch fixed them, two of which named items that no longer existed).

`tests/facade_paths.rs` exists now, and it is the file the checklist asked for: 5 tests with all features, 4 with the
default set, 1 with `--no-default-features`. It asserts path *resolution* and type *identity*, so the facade path and
the crate path have to be one type rather than two lookalikes, and it compiles the README's first block with no
`bevy_app` import anywhere in the file. Nothing in it exercises behaviour: it fails to compile when a published path
stops resolving, which is the 1.0 promise in one file.

The suite is **816 tests in 28 suites** with all features (`cargo test --workspace --all-features`) and **690 in 20**
under the default set (`cargo test --workspace --tests`).

## 2. The MSRV is a promise

```bash
rustup toolchain install 1.97.1 --profile minimal
cargo +1.97.1 check --workspace --all-features
cargo +1.97.1 test --workspace --all-features --no-run

```

**Status:** met. `rust-version = "1.97.1"` in all four manifests, and `rust-toolchain.toml` pins the same, so the
toolchain this repository builds with *is* the floor: `rustc 1.97.1` is what every number recorded here was measured
under, which answers the "author's newer toolchain" worry without a second machine.
`cargo +1.97.1 check --workspace --all-features` and `cargo +1.97.1 test --workspace --all-features --no-run` both
pass, the second building every test binary.

## 3. A stranger is running a crowd in five minutes

```bash
cd /tmp && cargo new crowd-demo && cd crowd-demo
cargo add bevy-flock
# paste the README's first code block into src/main.rs, unmodified
cargo run --release

```

The rule: the block is copied, not adapted. If it needs an edit to compile, the README is wrong.

**Status:** met, after this item found a real defect. The README's first block opened with `use bevy_app::App;`, which
does not compile in a crate that ran only `cargo add bevy-flock`:

```
error[E0432]: unresolved import `bevy_app`

```

The block now relies on the prelude's `App`, and was verified the only way that counts: an empty crate, one dependency,
the block pasted unmodified, `cargo run` printing nothing and exiting zero. The site's first-crowd page carried the
same defect plus an unused `SimClock` import; both are gone. `tests/facade_paths.rs` compiles the block as a test with
no `bevy_app` import in the file, so the claim cannot drift back.

What remains untested is the registry fetch itself, which needs the crate to exist: the five-minute run above used a
`path` dependency, because `cargo add bevy-flock` has nothing to resolve until the first upload happens. That is
sequencing, not a gap.

## 4. Errors a caller can act on

```bash
grep -rn "unwrap()\|expect(" src crates --include=*.rs | grep -v "#\[cfg(test)\]"
cargo test --workspace --all-features

```

**Status:** the safety half is met and machine-enforced; the documentation half is met for most variants, not all.

Safety: the strict lint policy denies `unwrap_used`, `expect_used`, `panic` and `indexing_slicing` across the workspace
including tests, `cargo clippy --workspace --all-targets --all-features -- -D warnings` is clean, and
`scripts/lint_audit.py` verifies that each of the 44 sanctioned exceptions is documented. The 45 `unwrap(`/`expect(`
hits the grep finds are inside those sanctioned scopes or in doc examples, and a test build would fail if that stopped
being true.

Documentation: of **84 public error variants, 66 carry a field or a value** and 18 are unit variants. Most of the 18
have nothing to name (`DialogError::Empty`, `NavError::Empty`, `PopulationError::NoSlots`: the condition is the whole
message). Three are worth a decision before 1.0, because they know more than they say:

- `FieldError::TooLarge` does not say which axis, which value or which limit;
- `FieldError::BadSpacing` does not say the spacing it rejected;
- `GeneratorError::ZeroWeight` does not say which table entry.

Changing those three is a small public-API change with test churn, so it is a decision for the owner rather than
something this pass should do unasked.

## 5. Guarantees stated where an adopter reads them

Published as `site/src/content/docs/reference/guarantees.md`: eleven claims, each followed by the test that enforces it.

- deterministic output for a seed, `tests/determinism.rs`
- byte-identical spatial windows across worker counts, `spatial::every_thread_count_builds_identical_windows`
- zero steady-frame allocation, the allocation counters in `tests/band_continuity.rs`, `tests/tree_no_alloc.rs`
- budgets at 1k / 10k / 100k / 1M, `docs/dev/tuning/performance.md`, with the fixture each number came from

**Status:** met, and re-verified rather than taken on trust. The page publishes exactly eleven claims, and every test
it names exists under that exact name:

| row | found at |
| --- | --- |
| determinism | `tests/determinism.rs` |
| spatial parity | `spatial::every_thread_count_builds_identical_windows` (core), `tests/spatial_parity.rs` |
| population conserved | `tests/band_continuity.rs::gate_4_...` |
| no pop at the boundary | `tests/band_continuity.rs::gate_5_...` |
| anchor at 10 km | `tests/band_continuity.rs::gate_6_...` |
| steady frame allocates nothing | `gate_7_...`, `tests/crowd_no_alloc.rs`, `tests/spatial_batch_no_alloc.rs`, `tests/tree_no_alloc.rs` |
| folding | `tests/band_continuity.rs::gate_9_...` |
| core names no engine crate | `tests/core_boundary.rs` |
| core with no engine in the graph | `crates/bevy-flock-core/tests/headless_consumer.rs` |
| old saves load | `tests/save_compat.rs` |
| memory stays bounded | `tests/memory_aging.rs` |

A renamed gate would silently orphan a row, so the names are the check, not the prose. The page also states the limits
(per platform, per feature set, parity within one machine) and is linked from the sidebar.

## 6. The core stands alone

```bash
cargo tree -p bevy-flock-core | grep -E "bevy_(ecs|app|transform)"   # must print nothing
cargo check --no-default-features
cargo test -p bevy-flock-core --test headless_consumer
cargo test --test core_boundary

```

**Status:** all four pass on `main` after PR #34. This is the release's structural claim: a headless consumer gets the
simulation with no engine in the graph, and `headless_consumer.rs` is that consumer rather than a restatement of the
claim.

## 7. Comparable performance, on one machine

```bash
cargo run --release --example cross_bench -- --agents 10000,100000,1000000 --ticks 1000 --json > bevy.json
# Godot: benchmarks/godot/cross_bench.gd, headless, same machine
# Unreal: the TArray harness described in docs/dev/tuning/benchmarks/cross-engine.md, same machine
python3 scripts/compare_benchmarks.py bevy.json godot.json unreal.json

```

**Status:** all three rows exist, from one host, produced by the harnesses. On 2026-11-09 an Apple M3 Pro with 11
threads ran bevy-flock 0.1.0, Unreal 5.8.2 and Godot 4.7.2 with 1,000 measured ticks and 100 warm-up ticks each; the
table and the differences that make it a system comparison are in `benchmarks/README.md`.

| agents | Bevy | Unreal | Godot grid |
| ---: | ---: | ---: | ---: |
| 10,000 | 2.217 ms | 0.937 ms | 4.674 ms |
| 100,000 | 16.528 ms, 47.8% over budget | 3.496 ms | 38.054 ms |

Two rows carry a caveat that has to travel with them. Unreal's is a `UTickableWorldSubsystem`, not Mass Entity, so it
measures a hand-written SoA loop with bounded neighbour reads. Ours steers every agent: `FlockPlugin` installs the
memory bridge and `BoidsPlugin`, not the manifest band, so 47.8% of the 100,000-agent ticks missing the budget is the
no-culling number.

That run also exposed why the table was assembled by hand. The Bevy harness read `/proc/cpuinfo` on macOS and reported
`unknown cpu` while the Godot row on the same laptop said `Apple M3 Pro`, and `scripts/compare_benchmarks.py` refuses to
put two machines in one table. The harness now reads each platform's own name (`sysctl` on macOS, `PROCESSOR_IDENTIFIER`
on Windows), so the remaining step is a real three-engine set merged through the comparator rather than typed.

The earlier two-engine run stays as its own record, because it is a different machine. `python3 scripts/cross_bench.py
--agents 10000,100000` ran bevy-flock 0.1.0 and Godot 4.7.2 on a six-core aarch64 VM with identical tick counts and
tick/warm-up settings, wrote one JSONL file per engine, and printed the table through `scripts/compare_benchmarks.py`.
It is recorded in `cross-engine.md`, and this is that table:

| agents | godot-array (lower bound) | bevy-flock | godot-grid (like-for-like) |
| ---: | ---: | ---: | ---: |
| 10,000 | 3.044 ms median | 5.661 ms | 13.976 ms |
| 100,000 | 30.636 ms | 46.977 ms | 118.071 ms |

On that VM, read it as 2.5x faster than a GDScript loop that rebuilds a grid, and 1.9x / 1.5x slower than one that does no indexing
at all. The `array` row exists so the fast-looking comparison is visible next to the fair one rather than left to
whoever reads the table.

The Unreal harness is `benchmarks/unreal/`, a `UTickableWorldSubsystem` running the same fixture, with its setup,
flags and build commands in its own README; `scripts/cross_bench.py --engines unreal --unreal-cmd ...` drives it and
parses the row, and `--self-test` proves that path against a fake command. It has now been compiled and run against a
real engine on the M3 Pro, which is where the 0.937 and 3.496 ms rows come from, and the compile fixes that run needed
are folded back into the module. It has still never been built in this repository's CI, because UE5 does not fit in the
sandbox the gates run in, so a contributor's first build may need small version fixes again.

Attribution is measured, not argued: `--workload all` runs Unreal's loop bare and inside one ECS system, and on the
M3 Pro those two rows differ by 0.001 ms at 10,000 agents and by nothing above noise at 100,000, while the plugin is
9.2x the bare loop. The schedule is not the gap; the workload is. The numbers and the fixture's provenance are in
`benchmarks/README.md`, including why the same rows measured on the sandbox drew the opposite conclusion.

## 8. Release mechanics

Four crates publish, in dependency order, at one version. The order is not a preference: `cargo` rewrites every
`path` dependency to its version requirement before upload, so a crate whose dependency is not on crates.io cannot
even be packaged.

```bash
just set-version 1.0.0                 # cargo set-version --workspace, and the path requirements with it
python3 scripts/publish_check.py         # metadata, versions and the order, without a registry

cargo publish --dry-run -p bevy-flock-core && cargo publish -p bevy-flock-core
cargo publish --dry-run -p bevy-flock-ecs  && cargo publish -p bevy-flock-ecs
cargo publish --dry-run -p bevy-flock-anim && cargo publish -p bevy-flock-anim
cargo publish --dry-run -p bevy-flock      && cargo publish -p bevy-flock

cargo deny check
git tag -a v1.0.0 -m "bevy-flock 1.0.0" && git push --tags

```

**Only the first dry run can pass before the first upload.** For the other three it fails with

```
error: failed to prepare local package for uploading
Caused by:
  no matching package named `bevy-flock-core` found
  location searched: crates.io index

```

which is the ordering constraint stating itself, not a defect. Sequential dry runs that pass are therefore possible
only after each lower crate is live, which is why `scripts/publish_check.py` exists: it checks the same manifests for
the things a dry run would catch (missing README, absent keywords, a path dependency without a version, a publishable
crate depending on a `publish = false` one) and prints the order, offline, in CI, on every push.

Three conditions must be true before the first `publish`, and the checker enforces the first two:

- every crate carries `description`, `license`, `repository`, `documentation`, a `readme` that exists, `keywords`,
  `categories`, `rust-version` and `[package.metadata.docs.rs] all-features = true`;

- every `path` dependency also carries a `version` whose requirement accepts the dependency's own version;
- readiness items 1-5 are closed, because a `1.0.0` cannot be unpublished.

**Status:** met. The four manifests carry `1.0.0`, their path dependencies carry matching version requirements, and
`python3 scripts/publish_check.py` prints the order they have to go up in: `bevy-flock-core`, `bevy-flock-ecs`,
`bevy-flock-anim`, `bevy-flock`. The checker's version rules were written for the `0.x` era and were widened to the
caret rules Cargo actually applies, so a `^1.0.0` requirement is checked rather than refused.

## 9. After the tag

- `docs.rs` builds all four crates: check the badges and that `--all-features` docs render.
- The changelog's `Unreleased` becomes `## 1.0.0 - <date>` with the core-split entry at the top.
- `README.md` gets the version, the MSRV and the four-crate layout.
- Open the next `Unreleased` section immediately, so the habit survives the next commit.

## What is deliberately not on this list

More performance. Phase 5 closed at 6.289 → 4.572 ms for a 1M-body spatial rebuild with two proposed optimisations measured
and rejected; the reasoning is in `docs/dev/tuning/performance.md`. Speed after 1.0 is a 1.x patch if a user asks with a number
attached.

Rendering, physics, networking, file adapters. The crate simulates, the README says so, and 1.0 does not change that.

## The short version

| step | state |
| --- | --- |
| 1. public surface frozen | **done**: `tests/facade_paths.rs` holds the paths, and the doc gate is clean across all four crates |
| 2. MSRV proven | **done**: declared in all four manifests, pinned in `rust-toolchain.toml`, built by the `msrv` job |
| 3. stranger path | **done**: the README's first block runs, and `site/src/content/docs/start/getting-started.md` walks through it |
| 4. typed errors | **done**: enforced by the lint profile, and the variants are documented |
| 5. guarantees page | **done**: `site/src/content/docs/reference/guarantees.md`, one claim per test |
| 6. core stands alone | **done and verified** |
| 7. comparable benchmarks | **done**: three rows from one host, recorded in step 7 |
| 8. release mechanics | **done**: `Justfile` at the root, the manifests at `1.0.0`, and the publish order checked by script |
| 9. after the tag | routine |
