# Release checklist — 0.1.0, the one-million-inhabitant milestone

**Milestone:** a library that steers 1,000,000 inhabitants inside a 6 ms frame, with 5,000 of
them thinking and steering at 60 Hz, and a crowd model a game can plug behaviour, memory and
saving into without forking it.

**Decision date:** 2026-11-04 · **Version under sign-off:** `0.1.0` (last tag `v0.1.0-rc.1`)

## 1. Gate ledger

Gates are from ADR 0018; evidence lives in `docs/gates/continuum.md` and the test suites named.

| # | Gate | Target | Measured | Status | Evidence |
| --- | --- | --- | --- | --- | --- |
| 1 | 1M ambient, advection stride | < 0.5 ms | 0.315 ms | **PASS** | `benches/continuum.rs` |
| 1b | Manifest 2,000 identities | — | 10.5 µs | **PASS** | `benches/continuum.rs` |
| 2 | Conservation over 10,000 strides | exact | exact | **PASS** | unit test |
| 3 | Determinism of manifest ids | exact | exact | **PASS** | unit test |
| 4 | Parity vs full simulation | 1% cells, 5% velocity | passes | **PASS** | `tests/band_continuity.rs` |
| 5 | No popping at the boundary | < 2% count delta | passes | **PASS** | `tests/band_continuity.rs` |
| 6 | Anchoring at 10 km | entity after 10k ticks | passes | **PASS** | `tests/band_continuity.rs` |
| 7 | No allocation after construction | zero | zero | **PASS** | `tests/band_continuity.rs`, `tests/tree_no_alloc.rs`, `examples/memory_probe.rs` |
| 8 | 100k with 5,000 manifested | < 8 ms | 3.37 ms | **PASS** | `benches/band.rs` |
| 9 | Folded NPC matches no query | zero matches | passes | **PASS** | `tests/band_continuity.rs` |

Nine of nine pass. Two of them (6, 9) exist because the harness found real defects: anchors were
being steered ten kilometres into the grid edge, and the field could not cover its own fold
radius. Both are fixed and pinned.

## 2. Performance sign-off

All numbers are criterion medians on a shared cloud vCPU (x86_64, `--release`), so treat them
as ratios and shape rather than as a hardware benchmark. Re-measure on the target platform
before quoting them in marketing.

| Measurement | Value | Gate | Verdict |
| --- | --- | --- | --- |
| 1,000,000 band frame, `--features simd` | **3.78 ms** | < 6 ms | PASS (37% headroom) |
| 1,000,000 band frame, scalar | 4.75 ms | < 6 ms | PASS (21% headroom) |
| 100,000 with 5,000 manifested | 3.37 ms | < 8 ms | PASS |
| Packed steer, 50,000 agents | 56.5 ms (wide) / 138 ms (scalar) | — | reference only |
| Planner (100k think budget) | 84 µs | — | PASS |
| Reflex tier ceiling | 64,000 / tick | 64,000 | PASS, at the ceiling |

`docs/performance.md` carries the history: what each optimisation bought, and the three that
were measured, rejected and written down (index-order SIMD blocking, `mul_add` in the parity
path, the borrow-split inner loop).

### Performance verdict

**Signed off** for the milestone. The 6 ms gate is met by 37% with the wide kernel and 21%
without it, so the library does not depend on an optional feature to hit its headline number.

## 3. Memory sign-off

From `docs/memory.md` (`examples/memory_probe.rs` + cachegrind):

| Property | Scalar | Wide | Verdict |
| --- | --- | --- | --- |
| Steady-state allocations over 60 frames | 0 | 0 | PASS |
| Footprint per agent | 28 B | 32 B | documented |
| Footprint at 1,000,000 | 26.7 MiB | 30.5 MiB | PASS |
| D1 miss rate (20k agents) | 73.1% | 14.9% | documented |
| LL miss rate | 19.8% | 2.4% | documented |

The 32 bytes per agent is now a public number: `docs/adr/0024` treats it as the crate's cost
contract, and the README quotes it.

**Memory verdict:** signed off, with the write-side note in §6.

## 4. Correctness and determinism

- [x] `cargo test --all-features` — 268 library tests plus the integration suites
      (`spawning`, `determinism`, `cognition`, `save_compat`, `memory_aging`,
      `band_continuity`, `tree_no_alloc`).
- [x] Scalar and wide kernels assert bit equality (`the_vectorised_kernel_agrees_with_the_scalar_one`).
- [x] Same binary + platform + features + seed ⇒ identical state digest over 600 ticks
      (`tests/determinism.rs`).
- [x] Behaviour trees evaluate without allocating (`tests/tree_no_alloc.rs`).
- [x] Save format: v2 round-trips, v1 fixtures still load, an unknown section is rejected
      rather than mis-parsed (`tests/save_compat.rs`).
- [x] Social memory ages out, anchors are exempt from sweeps, growth stays bounded
      (`tests/memory_aging.rs`).

## 5. Packaging and documentation

- [x] MSRV 1.97.1 pinned in `rust-toolchain.toml`; CI builds the pinned toolchain as well as
      stable.
- [x] Feature matrix builds: default (no dependencies), `memory`, `serde`, `simd`, all four
      together.
- [x] `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check`
      clean.
- [x] Dual licence (MIT OR Apache-2.0), `deny.toml` policy, `SECURITY.md`, `CONTRIBUTING.md`,
      `CODE_OF_CONDUCT.md`.
- [x] Every public item documented; `#![warn(missing_docs)]` in force.
- [x] 23 ADRs, indexed in `docs/adr/README.md`, including the decisions that were reversed.
- [x] Examples run: `hello_flock`, `registry_telemetry`, `profile_band`, `memory_probe`,
      `horde_100k`, `tavern_dialog`, `custom_behavior`, `ambient_metropolis`.
- [x] Platform and feature audit recorded in `docs/platforms.md`, with the two
      `--no-default-features` failures it found fixed and a `wasm` CI job that keeps the
      table true.
- [x] `cargo package --locked` is warning-free and the crate ships its benches and
      examples, so a reader can run what the README claims.
- [x] The book (`docs/book/src`) covers zero-to-tuned in eight chapters, and the README's
      ten-minute verification section links every claim to the artifact that pins it.
- [ ] `cargo package` / `cargo publish --dry-run` — to run immediately before tagging.
- [ ] CHANGELOG entry for `0.1.0` moved out of `[Unreleased]`.

## 6. Known limitations, stated before anyone finds them

1. ~~**Manifest/fold churn.**~~ **Fixed** (ADR 0025, `tests/band_churn.rs`). The manifest
   window was square and so reached 1.41 radius into its corners, past the 1.15 radius fold
   bound; `seed_field` piled the population into the corner of the field; and the spawn point
   was never checked against the fold bound. The band now converges to its 5,000 target in
   twenty frames and then performs zero manifestations and zero folds for as long as the camera
   holds still. Gate 9 no longer needs its `target: 0` sidestep.
2. **The wide kernel is x86_64-and-aarch64-shaped.** `wide` picks SSE2 on x86_64 and NEON on
   aarch64; there is no AVX2 path and no wasm-simd path. On wasm the scalar kernel is the
   correct choice, and the feature is off by default for that reason.
3. **Write-side cache traffic.** The wide path writes `desired` in cell-major order, so D1 write
   misses roughly doubled (0.11 M → 0.20 M at 20k agents). Not a gate, but the next profiler
   run should look at it before anything larger is attempted.
4. **Density dependence.** The wide kernel's advantage scales with density (−59% at 50,000
   agents, −8% at 5,000, −6% at 1,000). A sparse crowd should not enable the feature expecting
   the headline number.
5. **Single-threaded.** Nothing in the band, scheduler or kernel is parallel. 1M inhabitants fit
   in one frame on one core; more than that needs a different decision, not a bigger constant.

## 7. Sign-off

| Area | Verdict | By |
| --- | --- | --- |
| Gates (9/9) | PASS | Nick Campbell |
| Performance (1M < 6 ms) | PASS — 3.78 ms wide, 4.75 ms scalar | Nick Campbell |
| Memory (0 allocs, 32 B/agent) | PASS | Nick Campbell |
| Determinism | PASS | Nick Campbell |
| Documentation and packaging | PASS except `cargo package` dry run | Nick Campbell |
| Known limitation 1 (churn) | **Closed** — fixed in ADR 0025, pinned by `tests/band_churn.rs` | Nick Campbell |

**Release 0.1.0 as the crowd milestone.** The churn finding that was blocking 1.0 is fixed and
covered; the remaining limitations are documented and accepted.
