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

There are two, both in the steering kernels, both tracked as `kernel-bounds-checks` in `plan.toml`:

| File | Lint | Why |
| --- | --- | --- |
| `src/application/packed.rs` | `indexing_slicing` | 44 index expressions, each bounded by a length check earlier in the same function. Converting them puts an unreachable arm inside every inner loop of the kernel — the one place where the compiler's proof of the bound is worth more than the lint. |
| `src/application/crowd.rs` (wide kernel only) | `indexing_slicing` | 24 index expressions in the `simd` lane code, same bound argument. The expectation is `cfg_attr(feature = "simd", …)` because the scalar and threaded roads walk iterators and trip nothing. |

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

`scripts/lint_audit.py` scans `src`, `tests`, `benches` and `examples` for `#[allow(clippy::…)]` and
`#[expect(clippy::…)]`, including ones wrapped in `cfg_attr`, and fails if it finds a lint that is
not on the sanctioned list in the script. It also fails if a sanctioned override disappears, so the
list cannot rot into a set of exceptions nobody needs. `--self-test` checks the detector against
fixtures; CI runs both.

Adding an override therefore takes three steps, in this order: a reason in the script, a plan task
that removes it, and a sentence here. None of the three is a file-level opt-out that a reviewer
would have to notice in a diff.
