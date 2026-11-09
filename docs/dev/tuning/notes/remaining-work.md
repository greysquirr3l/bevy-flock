# Remaining work state (working notes)

Repo: /home/ubuntu/bevy-flock. Branch main. Author: Nick Campbell <s0ma@protonmail.com>.
Toolchain on PATH: export PATH="$HOME/.cargo/bin:$PATH". Rust 1.97.1 pinned.

Updated 2026-11-05. The plan (`plan.toml`) is the contract; this file is what the last
session learned so the next one does not repeat it.

## Where the crate stands

`0.1.0` is no longer only a crowd simulator. It ships three paths:

* **Crowd**: boids and steering, deterministic spatial grid, packed and transposed SIMD
  kernels, bands, the nine-gate ambient continuum, budgeted spawning and placement.
* **Cognition**: virtual-cost think scheduler, tiered cognition, behaviour trees with the
  full node set, utility scoring, social memory (`memory` feature) and its save format.
* **Society**: procedural generator, groups and factions, encounter perception, dialog
  graphs, barks, the world director, and ports for physics, replication and rendering.

Proof in place: 443 library tests, the full integration suite, six property tests over
arbitrary inputs, four runnable examples, criterion benches for every hot path, clippy at
`-D warnings` across all targets and features, and a CI matrix that runs all of it plus
the scaling-regression gate and the cell-size gate.

Delivered in `d50b203`: archetype-generator, flow-field-pathing, utility-scoring,
group-faction-model, world-director, encounter-perception, dialog-graph-engine,
bark-system, dialog-provider-seam, navigation-port, physics-adapter, render-adapter,
networking-adapter, benchmark-suite, examples-showcase. ADRs 0035-0040 record why.

## Remaining plan tasks and what each still needs

| Task | What is left |
| --- | --- |
| `memory-in-cognition` | Blackboard keys (`memory_valence`, `memory_familiarity`, `memory_flags`) written once per think; the three memory leaves with declared costs; calibration re-pin; the eligibility rule for who is remembered when the player walks into a market |
| `observability-and-debug` | Diagnostics, ghost-row/orphan-slot checks and the hysteresis rate exist. Missing: the per-NPC "why did it do that" dump (tree path, tier, weight, last barks, memory summary) and debug gizmos behind a feature |
| `prelude-rustdoc-book` | Prelude and rustdoc are current; the mdbook is not written |
| `docs-site-and-readme` | README has the honest status table; a docs build, the comparison page with measurements and the "what this is not" section are missing |
| `property-and-fuzz-suite` | Six properties shipped; the fuzz target for the save decoder and the RON tree path is not |
| `cell-size-reopen-gate` | Gate, script and CI job shipped; nothing pending unless a future benchmark trips it |
| `release-and-community` | CI, licences, contributing, security and changelog in place; `0.1.0` publish and the adapter contribution path remain |
| `cross-platform-audit` | CI covers Linux, macOS and Windows; wasm check, feature-powerset run and cross-target determinism remain |

## Traps already paid for

* Bevy system parameters are by value. `clippy::needless_pass_by_value` is allowed at the
  crate level with the reason written next to it; do not "fix" a system signature.
* Leaves are `&self` and shared. Per-NPC state belongs on the blackboard or the cursor
  stack — a timer in a leaf field gives the whole crowd one timer.
* `NpcBlueprint` owns its strings. A `&'static str` field makes the type impossible to
  deserialise from a borrowed input, which is what broke the RON round trip.
* `plan.toml` dependencies must name **open** tasks. Naming a completed task fails
  validation with "unknown task dependency" — that is wiggum saying the edge is satisfied,
  not pending.
* A phase whose tasks all close needs an explicit `tasks = []`, or the file stops parsing
  as the plan expects.
* `cargo clippy --all-targets --all-features -- -D warnings` is the gate. Examples carry
  their own allow block, because an example's job is to be read.

## Conventions

- Lints: deny unsafe_code, missing_docs, clippy::unwrap_used/expect_used/panic/indexing_slicing
  in library code (tests and examples exempt with a stated reason).
- Every public item has docs; hex literals are grouped (`0x00D1_A106`).
- Tests live in `#[cfg(test)] mod tests` inside modules; integration tests in `tests/`.
- Commits: conventional messages, author Nick Campbell <s0ma@protonmail.com>.
