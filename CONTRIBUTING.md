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
in the same commit as the manifest change. The reason is `docs/dev/tuning/performance.md`:
the timings published there are only reproducible against an exact dependency
graph, so a silent resolution bump would quietly invalidate them.

### The same gate without Actions

CI is the authority, but it is not always available — a billing failure, an outage,
or a fork with Actions disabled leaves a merged change unverified. `scripts/ci_local.py`
reads `.github/workflows/ci.yml` and runs the `run:` steps itself, so there is no second
copy of the commands to drift:

```sh
python3 scripts/ci_local.py --list             # every job, and what it would run
python3 scripts/ci_local.py --job fmt          # one job, here
python3 scripts/ci_local.py --all --keep-going # everything this machine can run
python3 scripts/ci_local.py --check            # fail if a job cannot be reproduced locally
```

It skips the steps that provision a runner (`uses:`), refuses any job it cannot translate
faithfully instead of running part of it, and skips jobs pinned to another OS. `--check` runs in
CI, so a workflow that grows a shape the translator cannot follow fails there rather than quietly
going unverified. `release` is tag-gated and `deny` is an action with no shell step; both are
listed in the script with that reason.

The lint profile (all + pedantic + nursery + cargo + perf, with `unwrap_used`,
`expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment` and `suspicious`
denied) lives in `Cargo.toml`, and `docs/dev/lint/lint-policy.md` says what each exception
is for. Do not add `#[allow]`: a denied lint that reappears in a file is caught by
`scripts/lint_audit.py`, which runs in CI and fails on any override that is not on
its sanctioned list. If a lint is genuinely wrong for this domain, argue for it in
the `[lints.clippy]` table with a comment that explains why, so the exception is
reviewable in one place.

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

`ARCHITECTURE.md` and `docs/dev/adr/` explain the boundaries. The short version:
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

No attribution trailers. The history belongs to the person who made the
changes, and the `commit-msg` hook refuses a message carrying a
`Co-authored-by:` trailer. Keep the body — that is where the explanation
belongs: what changed, why, and what was verified.

## Git hooks

Install them once per clone:

```sh
./scripts/install-hooks.sh
```

The hooks live in [`scripts/`](scripts), so every machine runs the same
checks. The installer copies them into `.git/hooks/` and leaves any hook
that is not in that directory alone — `post-commit` and `post-merge` belong
to `coraline hooks install` and `tralfamadore onboard`.

### pre-push

Runs the two CI checks that are cheap enough to run on every push:

- `cargo fmt --all -- --check` — the same command the `fmt` job runs.
- `cargo audit` — over every committed `Cargo.lock`, which is both the
  workspace's and the compat crate's.

It takes about four seconds. The rest of CI takes minutes, and a hook that
takes ten minutes is a hook people disable.

Two decisions worth knowing about:

**Waivers come from [`deny.toml`](deny.toml).** `cargo audit` has no config
file, so the hook reads the `id` fields out of `[advisories].ignore` in
`deny.toml` and passes them as `--ignore`. There is one list, not two. If
you add a waiver to `deny.toml`, the hook honours it immediately; if you
only added it to a copy somewhere, CI and your pre-push would disagree.

**It denies `unsound`, `unmaintained` and `yanked`, not just plain
vulnerabilities.** `cargo audit` files those three under `warnings` rather
than `vulnerabilities`, and exits zero for them by default — so a hook that
read only `vulnerabilities.count` reported a lockfile carrying a
use-after-free advisory as clean. The hook passes the three `-D` flags and
reads every category out of the report, which is the same set `deny.toml`
denies. A hook that passes where CI fails is worse than no hook: it hands
out a clean bill of health on a graph CI would reject.

A yanked crate is reported separately from an advisory, because the two
have different remedies. An advisory can be waived in `deny.toml`; a
yanked crate has no advisory id to ignore, so the only fix is to update the
lockfile.

A failed advisory scan is a hard block, including the case where the scan
could not run at all. `cargo audit` exits 1 for a vulnerability *and* for a
failure to fetch the advisory database, and the hook tells the two apart by
whether the report is empty on stdout — a finding prints its report, a
fetch failure prints nothing. A scan that did not run is not a scan that
passed.

If `cargo-audit` is not installed the hook says so and skips that half
rather than failing; CI still catches it. Install it with
`cargo install cargo-audit --locked`.

`--no-verify` bypasses the hook. That is a guard against a habit, not a
security boundary — CI runs the same checks and does not honour it.

### commit-msg

Refuses a commit message carrying a `Co-authored-by:` attribution trailer.
The rule is about the message, so it lives in `commit-msg` rather than
`pre-commit`: a pre-commit hook runs before the editor opens, so the message
does not exist yet.

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
- **Re-baselining the scaling sweep is a reviewed commit.** `docs/dev/tuning/data/scaling-sweep.json` is what the
  `Scaling regression` job compares against (ADR 0034). If a change genuinely trades parallel
  efficiency for something else, run `scripts/scaling_sweep.sh docs/dev/tuning/data/scaling-sweep.json`, commit
  the raw rows with the reasoning, and say what got slower. Never re-baseline to clear a red build
  without explaining which trade-off was made. A sweep taken with the current script pins one CPU per
  physical core and records the machine's topology, so every row counts real cores; the committed
  baseline predates that and holds real-core widths 1 and 2, which is the range the gate compares.
- **Cap by threads, not by task count** (ADR 0033). `cap = nproc / threads_per_task`: a workspace
  build that spawns `cargo -j6` wants to run alone, a single-crate codegen task wants every core
  filled. Measure the task with `python3 scripts/group_cap.py --measure "<command>"`, then size the
  group with `--groups-from` (exact, needs wiggum) or `--check-plan` (authored tasks only).
- **The plan's groups are dependency-derived, not capacity-aware.** Group 1 schedules eleven tasks;
  taking them all at once on a small machine is slower than taking three.

`CELL_SIZE_GATE` and the other CI jobs are unaffected: each job has its own runner and cache.
