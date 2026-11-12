# Lint drift: what stable says that the pin cannot

The crate builds and lints on a pinned toolchain, 1.97.1, because that is Bevy 0.20's MSRV
(`rust-toolchain.toml`). Clippy on a newer stable knows lints that 1.97 does not, and sharpens
others, so a tree that is warning-free where it is built is not warning-free everywhere. This page
is the inventory of that gap, and `scripts/lint_drift.py` regenerates and checks it.

Two things make this page necessary rather than pedantic.

The first is that "we would fix it if we saw it" is not a policy. A warning that only exists on a
toolchain nobody builds with is invisible to review, survives every CI run, and lands in the commit
that moves the pin, the commit that should be a one-line version bump. Cataloguing it with a named
task is the difference between debt and a surprise.

The second is that this table is checked. `scripts/lint_drift.py --check` runs clippy under stable,
extracts every warning with its site, and fails if the result differs from the table below in either
direction: a warning that is not listed, or a listed warning that is gone. CI runs it in the
`lint-future` job, next to the probe for the three lints 1.97 cannot name.

<!-- lint-drift:start -->
clippy on **1.99.1** (`cargo +stable clippy --all-targets --all-features`)
reports **12 warnings** across 5 lints: `chunks_exact_to_as_chunks` × 1, `manual_slice_fill` × 1, `needless_borrows_for_generic_args` × 1, `suboptimal_flops` × 7, `tuple_array_conversions` × 2.

| Lint | Site | What it wants |
| --- | --- | --- |
| `chunks_exact_to_as_chunks` | `crates/bevy-flock-core/src/domain/navmesh.rs:349` | `as_chunks::<N>()`, which hands out `&[T; N]` instead of a slice to re-check |
| `manual_slice_fill` | `crates/bevy-flock-core/src/application/packed.rs:705` | `slice.fill(value)`, which is one call and vectorises |
| `needless_borrows_for_generic_args` | `crates/bevy-flock-core/src/domain/memory.rs:874` | pass the closure by value; the call already takes it by value |
| `suboptimal_flops` | `crates/bevy-flock-core/src/application/avoidance.rs:248` | `f32::mul_add`, which fuses the multiply and the add into one rounding |
| `suboptimal_flops` | `crates/bevy-flock-core/src/application/avoidance.rs:249` | `f32::mul_add`, which fuses the multiply and the add into one rounding |
| `suboptimal_flops` | `crates/bevy-flock-core/src/application/avoidance.rs:250` | `f32::mul_add`, which fuses the multiply and the add into one rounding |
| `suboptimal_flops` | `crates/bevy-flock-core/src/domain/spawn.rs:215` | `f32::mul_add`, which fuses the multiply and the add into one rounding |
| `suboptimal_flops` | `crates/bevy-flock-core/src/domain/spawn.rs:219` | `f32::mul_add`, which fuses the multiply and the add into one rounding |
| `suboptimal_flops` | `crates/bevy-flock-core/src/domain/temperament.rs:613` | `f32::mul_add`, which fuses the multiply and the add into one rounding |
| `suboptimal_flops` | `crates/bevy-flock-core/src/domain/temperament.rs:614` | `f32::mul_add`, which fuses the multiply and the add into one rounding |
| `tuple_array_conversions` | `crates/bevy-flock-core/src/application/packed.rs:534` | `<[T; N]>::into()`, or a `let [a, b] = …` binding, instead of `(v[0], v[1])` |
| `tuple_array_conversions` | `crates/bevy-flock-core/src/application/packed.rs:537` | `<[T; N]>::into()`, or a `let [a, b] = …` binding, instead of `(v[0], v[1])` |
<!-- lint-drift:end -->

## What is deliberately not here

The three lints the pin cannot name, `clippy::unnecessary_rest_pattern`,
`clippy::rest_pattern_accessible_field` and rustc's `raw_borrows_via_references`, are not drift,
because the tree is already clean for them: 51 sites were rewritten when the forward job was made to
work, and `lint-future` denies all three. Drift is what is left when the tree is still living with
the newer answer.

Style is not drift either. A newer clippy rewording a diagnostic, or a lint moving between groups,
changes nothing here: the script reads warnings, not the text clippy prints for them, and matches on
the lint's name and the site.

## Closing it

The order matters:

1. Fix the site, not the lint. None of them is a case where stable is wrong, and adding an
   `allow` would have to go through `SANCTIONED` with a reason, which is a heavier decision than the
   fix in every one of them.
2. Run `python3 scripts/lint_drift.py --update`, which rewrites the region above from a live run.
3. Keep the pinned job clean: `cargo clippy --all-targets --all-features -- -D warnings` on 1.97.1
   is what `lint-future` cannot see, and it is what the repo actually builds with.

When the table is empty and the pin has not moved, `lint-future` can be tightened from denying three
lints to `-D warnings`, and this page becomes a paragraph saying so.
