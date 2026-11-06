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

# Right about the code, wrong about the rule: satisfied at the boundary instead of suppressed.
needless_pass_by_value = "deny"
cast_precision_loss = "deny"
cast_possible_truncation = "deny"
cast_sign_loss = "deny"
cast_possible_wrap = "deny"
float_cmp = "deny"
significant_drop_tightening = "deny"
missing_const_for_fn = "deny"

[lints.rust]
unsafe_code = "deny"
missing_docs = "deny"
```

Three lints are missing from that list on purpose: `raw_borrows_via_references`,
`unnecessary_rest_pattern` and `rest_pattern_accessible_field` exist in 1.99 and not in the pinned
1.97 toolchain, and naming a lint a compiler does not know turns every downstream build into an
`unknown_lints` warning.

The `lint-future` job closes that gap. It probes each of the three on a throwaway file with
`+stable`, then runs `cargo +stable clippy --all-targets --all-features` denying the ones that exist
— so the tree is already clean for them, and moving the pin is a version bump instead of a sweep. It
denies *only* those three: a newer stable also carries newer nursery lints, and a forward check that
fails on tomorrow's diagnostics would be noise rather than a gate. Those are catalogued instead, each
with its site and what it wants, in [lint-drift.md](lint-drift.md) — and `scripts/lint_drift.py`
fails CI if that catalogue stops matching what stable reports, in either direction.

Everything in the job is pinned with `+stable` or `rustup run stable`, deliberately.
`rust-toolchain.toml` pins 1.97.1 and rustup honours that file over the installed default, so a bare
`cargo clippy` in a job that installs stable would quietly run 1.97, find no new lints, and check
nothing. The probe also fails loudly if the probe itself errors, because "the compile failed" and
"the lint does not exist" are different answers and only one of them is good news.

`-D warnings` promotes the `warn` groups, so `pedantic`, `nursery`, `cargo`, `perf` and `all` are
errors in practice. The `deny` list is the part that matters most: `unwrap`, `expect`, `panic` and
direct indexing are how a library that is supposed to survive a crowd of 100,000 turns a bad input
into a crashed game.

The last group is the one this page argues for. Those nine lints were `allow`ed with a paragraph of
justification each, and every one of those paragraphs was a description of a deviation rather than a
reason: "an `f32` simulation converts at audited boundaries" is an argument for a conversion module,
not for three hundred casts. They are denied now, the conversion module exists, and the exceptions
that remain are `#[expect]`s the audit checks by rule — see [The exceptions](#the-exceptions).

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

**Four**, and they are all the same lint: `unsafe_code`, in a file that installs a counting
allocator to prove a frame allocates nothing. `GlobalAlloc` is unsafe by definition and there is no
way to observe allocations from inside the crate under test, so the test has to own the allocator.

| File | Why |
| --- | --- |
| `tests/crowd_no_alloc.rs` | asserts the crowd kernel allocates nothing per frame |
| `tests/tree_no_alloc.rs` | asserts behaviour-tree evaluation allocates nothing |
| `tests/band_continuity.rs` | asserts the band boundary gates allocate nothing over 10,000 ticks |
| `examples/memory_probe.rs` | reports steady-state allocation for a developer running it |

They use `#![expect(unsafe_code, reason = "…")]` rather than `#![allow]`, so the day the allocator
goes away the build fails with "this lint expectation is unfulfilled" instead of carrying a
permission nobody needs. The crate itself still denies `unsafe_code`: `scripts/lint_audit.py` sees
these four by name and fails on a fifth.

Everything else was removed rather than documented. The last two clippy expectations were
`indexing_slicing` in `src/application/packed.rs` (44 index expressions) and in the wide kernel of
`src/application/crowd.rs` (18). Both were defensible — every index was bounded by a length check
earlier in the same function — but "defensible" is not the standard the manifest sets, so
`kernel-bounds-checks` removed them:

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

Two `#[allow(dead_code)]` attributes in `tests/save_compat.rs` went the same way: they guarded
fixture constructors nothing called, so the fixtures were deleted. An allow that protects code
nobody uses is a comment with a build cost.

Two notes on what the audit does *not* cover, both deliberate:

- **Doctests.** `cargo test` compiles the examples in `//!` blocks, but clippy's `-D warnings` does
  not reach them, so `something().expect("fresh registry")` survives in three doc examples. They
  are what a game will write at start-up, and a doc example that threads a `Result` through
  `Ok::<(), _>(())` to demonstrate a registry teaches the wrong shape.
- **Sibling crates.** `compat/` and `fuzz/` are their own packages with their own manifests, so they
  do not inherit `[lints]`; the audit scans their sources for overrides and CI builds them, but the
  spine itself stops at the crate boundary. Closing that is `sibling-crate-lint-spine` in
  `plan.toml`.

Everything else a reasonable reader might expect to see is decided in the manifest rather than in a
file, so it appears once and reads as policy. `scripts/lint_audit.py` holds the same list as
`TOLERATED` and fails if the manifest allows anything that is not in it, or stops allowing something
that is:

| Allowed | Why |
| --- | --- |
| `module_name_repetitions` | the domain vocabulary is the API; `NpcMemory` in `memory` reads better than `Memory` |
| `must_use_candidate` | a getter that is obviously pure does not need an attribute to say so |
| `missing_errors_doc` | every fallible constructor names its error type in `# Errors` prose |
| `missing_panics_doc` | the crate denies `panic`, so the section would be empty in every signature |
| `struct_excessive_bools` | `BoidsConfig` and friends are flags by design; splitting them hides the defaults |
| `multiple_crate_versions` | Bevy's dependency graph is not ours to flatten |

Seven. The list used to be fifteen, and the eight that left are the interesting part, because each
one was a rule that turned out to be *right about the code and wrong about the rule*, which is the
only case where an exception is correct at all:

| Was allowed | What replaced it |
| --- | --- |
| `needless_pass_by_value` | `#[expect]` on the system itself — `&Res<T>` is not a `SystemParam`, so the signature is the framework's |
| `cast_precision_loss` | `src/domain/num.rs`, which converts at one boundary and says what each conversion does |
| `cast_possible_truncation` | the same module, plus `u64_to_low_u32` where truncation *is* the operation (hashes, PRNG words) |
| `cast_sign_loss` | the same module: `i32_to_usize` and friends clamp the negative case in a branch |
| `cast_possible_wrap` | the same module |
| `float_cmp` | `.to_bits()` on both sides — exact comparison, spelled as bit equality, which is what a determinism test means |
| `significant_drop_tightening` | the benchmark group's scope is the measurement; the guard is dropped where the timing ends |
| `missing_const_for_fn` | `const` is now on the functions where it costs nothing, and the conversion helpers are `const` so a `const fn` can call them |

One of them came back. `suboptimal_flops` was denied, the kernels were fused by hand, and
`application::packed::tests::the_vectorised_kernel_agrees_with_the_scalar_one` failed within the
hour on a one-ulp difference: fusing the wide lane and not the scalar reference changes the result.
Fusion is a numerical decision, not a tidy-up, so the lint is allowed again with that reason and the
kernels stay unfused.

Three `#[expect]`s are therefore permitted, and the audit checks all three:

1. **In `src/domain/num.rs`, for a cast lint.** The module exists so a numeric conversion happens
   once, with its rounding written down, instead of at three hundred call sites. Every helper there
   carries an expectation naming the cast it owns.
2. **On a Bevy system, for `needless_pass_by_value`.** The parameter is `Res<T>`, `ResMut<T>`,
   `Query<…>`, `Commands`, `Local<T>`, `EventReader<T>`, `EventWriter<T>`, `Single<…>` or
   `ParamSet<…>`; the framework hands it over by value and a reference would not be a `SystemParam`.
   The audit reads the sixty lines below the attribute and only accepts the expectation when one of
   those types is in the signature.
3. **In `benches/`, for `significant_drop_tightening`.** Criterion writes its report when the
   benchmark group drops, so the group's scope *is* the measurement; shortening the guard's life
   would measure less, not more. The audit only accepts the expectation in a bench, and only when
   the reason names Criterion.

An `#[expect]` is not a suppression. It fails the build the day the lint stops firing, so it cannot
rot into an `allow`; it is a statement that the lint is wrong here, and the compiler holds you to it.
A bare `#[allow]` is never correct and the audit rejects one outright, `reason` or not.

## Policing the exceptions

`scripts/lint_audit.py` scans every Rust file in the tree — `src`, `tests`, `benches`, `examples`,
`compat`, `fuzz`, `demo`, `demo-gpu` — for `#[allow(…)]` and `#[expect(…)]` of **any** lint,
clippy's or rustc's,
including the ones wrapped in `cfg_attr`. Comments are blanked before scanning, because this page and
half the crate's doc comments talk about the overrides they forbid. An `#[allow]` that is not in
the script's `SANCTIONED` table fails the build; so does a `SANCTIONED` entry that has disappeared,
because an exception nobody needs is an exception nobody reads. An `#[expect]` is judged by the three
rules above instead: right file or right signature, and a reason.

The `reason = "…"` argument is stripped as a unit before an attribute is split on commas, so a
reason with a comma in it does not become a phantom lint name, and a name that is not an identifier
is ignored — which is how a `cfg_attr(` prefix's stray paren is dropped rather than reported.

It also reads every manifest that carries the table — the library's `Cargo.toml`, `demo/Cargo.toml`
and `demo-gpu/Cargo.toml` — and fails if the policy there is not the policy this page describes, in
both directions:

- **Denied stays denied.** `unsafe_code` and `missing_docs` must be `deny`, as must `suspicious`,
  `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`, `cast_ptr_alignment`, and the nine
  cast, float, `const` and `needless_pass_by_value` lints above; the `all`, `pedantic`, `nursery`,
  `cargo` and `perf` groups must still be `warn` for `-D warnings` to do its job. A clean tree under
  a weakened manifest is not a passing audit.
- **Allowed stays allowed, and nothing else.** The set of `allow` entries in the manifest must equal
  the `TOLERATED` table exactly, entry for entry, in both directions. Because a demo crate may be
  *stricter* than the library — the GPU demo denies the groups rather than warning about them, since
  it has no `-D warnings` step of its own — the comparison is against a strength order rather than
  for equality, and no manifest may allow a lint that `TOLERATED` does not list. The tolerated set
  itself is checked once against all three manifests together, so an entry nothing allows any more
  still fails.

`--self-test` checks the detector against fixtures rather than the tree: a clean file, a file-level
`allow`, an item-level `expect`, a `cfg_attr`-wrapped `expect`, a bare rustc lint in each of `allow`
and `expect`, a documented cast expectation inside the conversion module, the same expectation
without a reason, the same expectation in another file, a documented expectation on a Bevy system,
one on a function that is not a system, and a reason wrapped across lines with a trailing `\` — the
shape that used to be split on its commas and read back as a list of lint names; then the sanction
mechanism (a listed override passes, the same override unlisted fails, a stale entry fails); then
the manifest (a downgraded deny, a deny replaced by an allow, an undocumented allow, a tolerated
allow that vanished, an intact manifest, a second crate that is stricter than the library, and a
second crate that allows what the library does not). CI runs both.

Adding an override therefore takes three steps, in this order: a reason in the script, a plan task
that removes it, and a sentence here. None of the three is a file-level opt-out a reviewer would have
to notice in a diff.
