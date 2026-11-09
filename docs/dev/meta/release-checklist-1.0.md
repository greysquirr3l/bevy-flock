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

**Status:** the re-export is in place and **811 tests pass through it** (`cargo test --workspace --all-features`, 25
suites; 704 of them under the default feature set). The doc gate passes: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
--workspace --all-features` was red with nine broken or private links until the adoption-surface branch fixed them, two of
which named items that no longer existed.

`tests/facade_paths.rs` does not exist yet, write it before the tag, because it is the only test that fails if a published
path is accidentally renamed.

## 2. The MSRV is a promise

```bash
rustup toolchain install 1.97.1 --profile minimal
cargo +1.97.1 check --workspace --all-features
cargo +1.97.1 test --workspace --all-features --no-run
```

**Status:** `rust-version = "1.97.1"` in both manifests, `rust-toolchain.toml` pins the same. Not yet run on a machine that
has *only* 1.97.1, worth doing once, since a crate that builds on the author's newer toolchain says nothing about its floor.

## 3. A stranger is running a crowd in five minutes

```bash
cd /tmp && cargo new crowd-demo && cd crowd-demo
cargo add bevy-flock
# paste the README's first code block into src/main.rs, unmodified
cargo run --release
```

The rule: the block is copied, not adapted. If it needs an edit to compile, the README is wrong.

**Status:** done in the documentation release. `README.md` leads with the five-line block, then a ten-minute verification
path. Not yet re-run from a genuinely empty crate on a machine with no cached dependencies, which is the last confirmation
this step wants before the tag.

## 4. Errors a caller can act on

```bash
grep -rn "unwrap()\|expect(" src crates --include=*.rs | grep -v "#\[cfg(test)\]"
cargo test --workspace --all-features
```

**Status:** the strict lint policy already denies `unwrap_used`, `expect_used`, `panic` and `indexing_slicing` across the
workspace including tests, and `scripts/lint_audit.py` verifies that every sanctioned exception is documented. The
remaining question here is documentation, not safety: each public error variant should name a field and a value the way
`ConfigError::InvalidDistance { field, value }` does.

## 5. Guarantees stated where an adopter reads them

Published as `site/src/content/docs/reference/guarantees.md`: eleven claims, each followed by the test that enforces it.

- deterministic output for a seed, `tests/determinism.rs`
- byte-identical spatial windows across worker counts, `spatial::every_thread_count_builds_identical_windows`
- zero steady-frame allocation, the allocation counters in `tests/band_continuity.rs`, `tests/tree_no_alloc.rs`
- budgets at 1k / 10k / 100k / 1M, `docs/dev/tuning/performance.md`, with the fixture each number came from

**Status:** done. `site/src/content/docs/reference/guarantees.md` publishes eleven claims, each with the test that
enforces it, and states the limits of each (determinism is per platform and per feature set; parity is across worker counts
on one machine). The page is linked from the sidebar, so an adopter meets it before they meet an issue tracker.

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

**Status:** our row exists; the other two need one afternoon on a single machine. Until then the README says "in the band
the published Godot result occupies" and nothing stronger. The comparison script refuses to mix machines, that refusal is
the feature.

## 8. Release mechanics

```bash
# Both crates move together: the facade pins the core by version and path, so a published facade cannot drift from it.
# Edit both manifests to 1.0.0, or use `cargo set-version --workspace 1.0.0`.
cargo publish --dry-run -p bevy-flock-core
cargo publish -p bevy-flock-core
cargo publish --dry-run -p bevy-flock
cargo publish -p bevy-flock
cargo deny check
git tag -a v1.0.0 -m "bevy-flock 1.0.0" && git push --tags
```

Order matters: the core publishes first because the facade depends on it by version. Two conditions must be true before the
first `publish`:

- `publish = false` is removed from `crates/bevy-flock-core/Cargo.toml`, and its `description`, `keywords`, `categories`,
  `readme` and `[package.metadata.docs.rs]` are filled in the way the facade's are.
- Readiness items 1–5 are closed, because a `1.0.0` cannot be unpublished.

**Status:** not ready, the core is `publish = false` by design during phase 6.

## 9. After the tag

- `docs.rs` builds both crates: check the badge and that `--all-features` docs render.
- The changelog's `Unreleased` becomes `## 1.0.0 - <date>` with the core-split entry at the top.
- `README.md` gets the version, the MSRV and the two-crate layout.
- Open the next `Unreleased` section immediately, so the habit survives the next commit.

## What is deliberately not on this list

More performance. Phase 5 closed at 6.289 → 4.572 ms for a 1M-body spatial rebuild with two proposed optimisations measured
and rejected; the reasoning is in `docs/dev/tuning/performance.md`. Speed after 1.0 is a 1.x patch if a user asks with a number
attached.

Rendering, physics, networking, file adapters. The crate simulates, the README says so, and 1.0 does not change that.

## The short version

| step | state |
| --- | --- |
| 1. public surface frozen | re-export in place; `facade_paths.rs` missing |
| 2. MSRV proven | declared and pinned; not built on 1.97.1 alone |
| 3. stranger path | example exists; README not updated |
| 4. typed errors | enforced by lints; a few variants need docs |
| 5. guarantees page | properties tested; page missing |
| 6. core stands alone | **done and verified** |
| 7. comparable benchmarks | machinery done; two rows missing |
| 8. release mechanics | blocked on 1–5 |
| 9. after the tag | routine |
