# Contributing

Thanks for considering it. This crate has an unusually strict definition of
"done", and it is written down so a review is not a surprise.

## Preflight

Every change must pass, in this order:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo test --locked --no-default-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
```

Dependencies are locked, and CI enforces it: `Cargo.lock` and `compat/Cargo.lock`
are committed and every job runs `--locked`, with a `lockfile` job that fails if
either has gone stale. If you change a dependency, commit the regenerated lockfile
in the same commit as the manifest change. The reason is `docs/performance.md`:
the timings published there are only reproducible against an exact dependency
graph, so a silent resolution bump would quietly invalidate them.

The lint profile (all + pedantic + nursery + cargo + perf) and its documented
exceptions live in `Cargo.toml`. Do not add `#[allow]`; if a lint is genuinely
wrong for this domain, argue for it in the `[lints.clippy]` table with a comment
that explains why, so the exception is reviewable in one place.

## The six rules

1. **No panics in library code.** No `unwrap`, `expect`, `panic!`, `todo!`, or
   panicking index outside tests. If an invariant cannot be expressed in the type
   system, return a `Result` or fail the node and keep simulating.
2. **No allocation in steady state.** Per-frame work uses buffers that are
   allocated once (`Local<Vec<_>>`, reused grid storage). A benchmark that shows
   allocator traffic in a steady-state frame is a bug report.
3. **Determinism is not negotiable.** No `HashMap` iteration in simulation
   logic, no wall-clock time, no `std` transcendentals (`libm` only), no
   reliance on `Entity` indices or pointer order. If a change cannot be made
   deterministic, it does not ship.
4. **Every public item is documented**, with `# Errors` for fallible functions
   and a doc example where a reader would otherwise need two.
5. **Every new behaviour has a test that fails without it.** "It works" is
   demonstrated by a red-then-green test, not by a description.
6. **No feature without a plan entry.** `plan.toml` is the specification. If you
   are building something not described there, add the task first, with its
   capability contract and acceptance scenario.

## Architecture

`ARCHITECTURE.md` and `docs/adr/` explain the boundaries. The short version:
`domain/` is pure and knows nothing about Bevy, time, or I/O; adapters translate.
A pull request that puts a `Transform` in `domain/` will be asked to move it.

## Commit messages

Conventional commits, scope where useful:

```
feat(boids): priority-weighted steering accumulation
fix(spatial): clamp out-of-bounds agents instead of dropping them
perf(spatial): reuse the counting-sort cursor across rebuilds
docs(adr): record the libm decision
```

The body explains *why*, and past tense for what changed is fine. Reference the
plan task id (`T12`) when the commit implements one.

## Benchmarks

Performance claims need numbers. Run:

```sh
cargo bench --bench crowd
```

and paste the relevant table into the pull request. A change that makes
`flock_frame/10000` slower needs a very good argument.

## Running plan tasks concurrently

wiggum schedules some tasks in the same execution group. Before taking one, read ADR 0032 — it is
short and it is measured:

- **Independent file ownership only.** Two tasks may not write the same file.
- **One committer per group.** The others leave the tree dirty. Concurrent `git add` fails with
  `index.lock` about two times in three, silently, if the script does not check exit codes.
- **Mixed feature sets are fine.** Cargo rebuilds this crate's units, not the dependency tree: 3 s of
  churn, not 3 minutes.
- **Benchmark-bearing tasks get their own group**, because a benchmark sharing a machine with three
  compilations measures the machine.
- **Cap compile-heavy groups at about half the cores.** Three tasks on six cores already run at 33%
  efficiency, and `CARGO_TARGET_DIR` isolation does not recover it (46 s isolated against 43 s
  shared, measured). The shared target directory is the right default; isolation is a debugging tool.
- **The plan's groups are dependency-derived, not capacity-aware.** Group 1 schedules eleven tasks;
  taking them all at once on a small machine is slower than taking three.

`CELL_SIZE_GATE` and the other CI jobs are unaffected: each job has its own runner and cache.
