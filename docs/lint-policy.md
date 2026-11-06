# Lint policy

The crate is linted at one setting, in one place: `[lints]` in `Cargo.toml`. CI runs
`cargo clippy --all-targets --all-features -- -D warnings`, and again with `--no-default-features`,
so a warning anywhere — in `src`, `tests`, `benches` or `examples` — is a build failure.

This page says what is enabled, why each exception exists, and how the exceptions are policed.

## What is denied

```toml
[lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
nursery = { level = "warn", priority = -1 }
cargo = { level = "warn", priority = -1 }
perf = { level = "warn", priority = -1 }

unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
indexing_slicing = "deny"
cast_ptr_alignment = "deny"
suspicious = "deny"

[lints.rust]
raw_borrows_via_references = "deny"
```

`-D warnings` promotes the `warn` groups, so `pedantic`, `nursery`, `cargo`, `perf` and `all` are
errors in practice. The `deny` list is the part that matters most: `unwrap`, `expect`, `panic` and
direct indexing are how a library that is supposed to survive a crowd of 100,000 turns a bad input
into a crashed game.

Two lints from the target profile do not exist in clippy 0.1.97 —
`unnecessary_rest_pattern` and `rest_pattern_accessible_field` are 1.99 additions. The manifest
cannot name a lint the running toolchain does not know, so the dedicated CI job turns them on when
the nightly/stable in use supports them.

## How a test fails

Tests may not unwrap either. A fixture that cannot build has nothing to assert, so the failure is
reported through `bevy_flock::testkit`:

```rust
use bevy_flock::testkit::OrDie;

let grid = SpatialGrid::try_new(config).or_die();
```

`or_die` reports `unreachable!("the fixture should build, and it reported: …")` with the caller's
location, so the message is the one the test wrote rather than `called Option::unwrap() on a None
value`. `OrDieErr` is the mirror for a constructor whose refusal is the assertion. `GetOrDie` and
`GetOrDieMut` cover slice and index access.

`src/testkit.rs` is `#[doc(hidden)]` and is not part of the simulation API.

## The exceptions

**There are none.** Every file-level `#[allow]` and `#[expect]` in the tree has been removed, the
steering kernels included.

The last two were `indexing_slicing` in `src/application/packed.rs` (44 index expressions) and in
the wide kernel of `src/application/crowd.rs` (18). Both were defensible — every index was bounded
by a length check earlier in the same function — but "defensible" is not the standard the manifest
sets, so `kernel-bounds-checks` removed them instead of documenting them:

- Values that were `buffer[index]` became `buffer.get(index)`, with a `let … else` arm for the case
  that cannot happen. This is not extra work when the bound sits outside a loop — but the first
  attempt put a `get` at the top of the neighbour scan and measured **19% slower** on a dense crowd:
  seven `Option<&f32>` in a tuple pattern, each with its own exit branch, is not seven subscripts
  whose checks the optimizer merges. The fix was to slice the bucket's range once per cell and walk
  the rows, which removed the check from the loop entirely and came out 2% *ahead* of where the
  kernel started ([ADR 0049](adr/0049-row-major-neighbour-broadcast.md)).
- The four-lane tails stopped indexing their `[f32; 4]` results at all: a `lane!(0)..lane!(3)` macro
  spells a literal index once per lane, which the lint exempts because the length is known, and the
  four records are then walked as an array of `NeighbourSums`.
- The wide kernel's neighbour scan reads a whole row per agent from one buffer instead of seven
  parallel component arrays, so a neighbour is one contiguous 28-byte load rather than seven.

`scripts/lint_audit.py` still lists a `SANCTIONED` table, and it is empty. The table is the
mechanism, not an artefact: adding an override means adding it there, with a reason and a
`plan.toml` task that removes it.

Two notes on what the audit does *not* cover, both deliberate:

- **Doctests.** `cargo test` compiles the examples in `//!` blocks, but clippy's `-D warnings` does
  not reach them, so `something().expect("fresh registry")` survives in three doc examples. They
  are what a game will write at start-up, and a doc example that threads a `Result` through
  `Ok::<(), _>(())` to demonstrate a registry teaches the wrong shape.
- **`#[deny]` attributes.** A file that turns a lint *up* is not an override; the audit only looks
  for `allow` and `expect`.

Everything else that a reasonable reader might expect to see is decided in the manifest rather than
in a file, so it appears once and reads as policy:

| Allowed | Why |
| --- | --- |
| `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc` | naming and docs preferences the crate does not want to enforce mechanically |
| `struct_excessive_bools`, `multiple_crate_versions` | Bevy's dependency graph is not ours to flatten |
| `needless_pass_by_value` | Bevy system parameters (`Res`, `Query`, `Commands`) are taken by value by construction |
| `cast_precision_loss`, `cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap` | an `f32` simulation converts at audited boundaries; `cast_lossless` is *not* allowed, so every widening cast uses `From` |
| `suboptimal_flops` | fusing into `mul_add` changes rounding, and the formulas are pinned by bit-exact tests |
| `float_cmp` | exact comparison is the substance of the determinism tests |
| `significant_drop_tightening` | Criterion's `BenchmarkGroup` is a guard by design |
| `missing_const_for_fn` | `const` is added where it enables a static, not for its own sake |

## Policing the exceptions

`scripts/lint_audit.py` scans every crate in the workspace — `src`, `tests`, `benches`, `examples`,
`compat`, `fuzz` — for `#[allow(clippy::…)]` and `#[expect(clippy::…)]`, including the ones wrapped
in `cfg_attr`, and fails on any lint that is not in its `SANCTIONED` table. It fails in the other
direction too: a sanctioned override that has disappeared is an entry to delete, so the table cannot
rot into a set of exceptions nobody needs. Comments are blanked before scanning, because this
crate's documentation talks about the overrides it forbids.

`--self-test` checks the detector against fixtures, including the three that matter for the
mechanism: a sanctioned override passes, the same override unlisted fails, and a stale sanction
fails. CI runs both.

Adding an override therefore takes three steps, in this order: a reason in the script, a plan task
that removes it, and a sentence here. None of the three is a file-level opt-out that a reviewer
would have to notice in a diff.
